#!/usr/bin/env python3
"""Agent mapping trials: a model authors MappingIR, the real checker admits or refuses, and we iterate.

The gateway URL and token come from the environment at run time and are never stored. The checker
transport is `cargo_transport` (the ignored `bench_mapping` test binary); per-token prices come from
fixtures/bench/mapping-trial-prices.json. Rows and transcripts stay in the caller's scratch directory.

    cargo test --locked -p norn-cell --test bench_mapping --no-run   # prints the test binary path
    NORN_BENCH_GATEWAY_URL=... NORN_BENCH_GATEWAY_TOKEN=... python3 scripts/bench_mapping_trials.py \
        --binary target/debug/deps/bench_mapping-HASH --rows ROWS.json --scratch DIR \
        --models google/gemini-3.8-flash,openai/gpt-5.4-mini --repeats 3 --cap 20

ROWS.json maps each task's `rows` name (olist_products, olist_customers, olist_sellers, uci_lines) to a
list of source rows.
"""
import argparse
import collections
import concurrent.futures
import http.client
import itertools
import json
import math
import os
from pathlib import Path
import re
import subprocess
import threading
import time
import urllib.error
import urllib.request

MAX_ROUNDS = 5
MAX_COMPLETION = 6000
ITEM_CONTRACT = {"entity": "canon.Item.entity", "interface": "canon.Catalog.interface",
                 "properties": {"id": "text", "code": "text", "label": "text"}}
PARTY_CONTRACT = {"entity": "canon.Party.entity", "interface": "core.Agent.interface",
                  "properties": {"display_name": "text, 1 to 200 characters",
                                 "kind": "enum: 'person', 'organization' or 'system'"}}
OLIST_PRODUCT = [("product_id", "string", True), ("product_category_name", "string", False),
                 ("product_name_lenght", "integer", False), ("product_description_lenght", "integer", False),
                 ("product_photos_qty", "integer", False), ("product_weight_g", "integer", False),
                 ("product_length_cm", "integer", False), ("product_height_cm", "integer", False),
                 ("product_width_cm", "integer", False)]
OLIST_CUSTOMER = [("customer_id", "string", True), ("customer_unique_id", "string", True),
                  ("customer_zip_code_prefix", "string", True), ("customer_city", "string", True),
                  ("customer_state", "string", True)]
OLIST_SELLER = [("seller_id", "string", True), ("seller_zip_code_prefix", "string", True),
                ("seller_city", "string", True), ("seller_state", "string", True)]
UCI_LINE = [("Invoice", "string", True), ("StockCode", "string", True), ("Description", "string", False),
            ("Quantity", "integer", True), ("InvoiceDate", "string", True), ("Price", "decimal", True),
            ("CustomerID", "string", False), ("Country", "string", True)]
TASKS = {
    "olist_products": dict(rows="olist_products", entity="canon.Item", contract=ITEM_CONTRACT, fields=OLIST_PRODUCT,
                           brief="Map each product to one catalog Item. The label is the product's category."),
    "olist_customers": dict(rows="olist_customers", entity="canon.Party", contract=PARTY_CONTRACT, fields=OLIST_CUSTOMER,
                            brief="Map customers to Party, one Party per real person. Dataset notes: customer_id is the key "
                                  "to the orders table and every order has its own customer_id; customer_unique_id identifies "
                                  "the customer."),
    "olist_sellers": dict(rows="olist_sellers", entity="canon.Party", contract=PARTY_CONTRACT, fields=OLIST_SELLER,
                          brief="Map each seller to one Party. Sellers are businesses."),
    "uci_items": dict(rows="uci_lines", entity="canon.Item", contract=ITEM_CONTRACT, fields=UCI_LINE,
                      brief="These are invoice lines. Map each line's product to one catalog Item. The label is the description."),
    "uci_customers": dict(rows="uci_lines", entity="canon.Party", contract=PARTY_CONTRACT, fields=UCI_LINE,
                          brief="These are invoice lines. Map each line's customer to one Party, a person. Lines without a "
                                "customer must not create a Party."),
}
# Blind variants drop the hints, so identity and absence must be inferred from the rows.
TASKS["olist_customers_blind"] = dict(TASKS["olist_customers"], brief="Map customers to Party.")
TASKS["uci_customers_blind"] = dict(TASKS["uci_customers"], brief="These are invoice lines. Map each customer to Party.")
GUIDE = """You author Norn MappingIR. Return only a JSON object {"maps":[...]} with no prose.
Each map has: "id" (string), "source":{"dataset":"Rows"}, "target":{"entity":E,"interface":I},
"subject" (expression giving the target object's key), "sourceRecord" (expression giving the source record id),
"properties":[{"property":P,"value":expression}, ...].
Expressions read the source row as input.<field>. String literals use single quotes. `a ?? b` supplies a value for a
nullable a. `+` concatenates two strings. String functions: lower, upper, trim, take(text, n), contains, startsWith,
endsWith, len. Number functions: abs, floor, ceil, round, trunc. There is no general type conversion function.
`c ? a : b` is the conditional. Every source field not marked required may be null.
Example for a different source:
{"maps":[{"id":"row","source":{"dataset":"Rows"},"target":{"entity":"canon.Item.entity","interface":"canon.Catalog.interface"},
"subject":"input.sku","sourceRecord":"input.sku","properties":[{"property":"id","value":"input.sku"},
{"property":"code","value":"input.sku"},{"property":"label","value":"input.title ?? ''"}]}]}"""

# Hand-written candidates. Structural traps must refuse; semantic traps show what the checker cannot know.
def _map(entity, interface, subject, properties, when=None):
    row = {"id": "row", "source": {"dataset": "Rows"}, "target": {"entity": entity, "interface": interface},
           "subject": subject, "sourceRecord": subject,
           "properties": [{"property": k, "value": v} for k, v in properties]}
    return [{**row, "when": when} if when else row]
ITEM = ("canon.Item.entity", "canon.Catalog.interface")
PARTY = ("canon.Party.entity", "core.Agent.interface")
GOOD_ITEM = [("id", "input.product_id"), ("code", "input.product_id"), ("label", "input.product_category_name ?? ''")]
TRAPS = [
    ("unknown_source_field", "structural", "olist_products", _map(*ITEM, "input.nope", GOOD_ITEM)),
    ("unknown_target_property", "structural", "olist_products", _map(*ITEM, "input.product_id", GOOD_ITEM + [("price", "input.product_id")])),
    ("missing_required_property", "structural", "olist_products", _map(*ITEM, "input.product_id", GOOD_ITEM[:2])),
    ("nullable_into_required", "structural", "olist_products", _map(*ITEM, "input.product_id", GOOD_ITEM[:2] + [("label", "input.product_category_name")])),
    ("number_into_text", "structural", "olist_products", _map(*ITEM, "input.product_id", GOOD_ITEM[:2] + [("label", "input.product_weight_g")])),
    ("invalid_enum_literal", "structural", "olist_sellers", _map(*PARTY, "input.seller_id", [("display_name", "input.seller_id"), ("kind", "'company'")])),
    ("wrong_interface", "structural", "olist_sellers", _map(PARTY[0], ITEM[1], "input.seller_id", [("display_name", "input.seller_id"), ("kind", "'organization'")])),
    ("unknown_entity", "structural", "olist_products", _map("canon.Widget.entity", ITEM[1], "input.product_id", GOOD_ITEM)),
    ("expression_syntax", "structural", "olist_products", _map(*ITEM, "input.product_id ++", GOOD_ITEM)),
    ("per_order_identity", "semantic", "olist_customers", _map(*PARTY, "input.customer_id", [("display_name", "input.customer_id"), ("kind", "'person'")])),
    ("fabricated_customer", "semantic", "uci_customers", _map(*PARTY, "input.Invoice", [("display_name", "input.Country"), ("kind", "'person'")])),
    ("wrong_party_kind", "semantic", "olist_sellers", _map(*PARTY, "input.seller_id", [("display_name", "input.seller_id"), ("kind", "'person'")])),
    ("swapped_columns", "semantic", "uci_items", _map(*ITEM, "input.Country", [("id", "input.Country"), ("code", "input.Country"), ("label", "input.StockCode")])),
]


REFERENCE = {
    "olist_products": _map(*ITEM, "input.product_id", GOOD_ITEM),
    "olist_customers": _map(*PARTY, "input.customer_unique_id", [("display_name", "input.customer_unique_id"), ("kind", "'person'")]),
    "olist_sellers": _map(*PARTY, "input.seller_id", [("display_name", "input.seller_id"), ("kind", "'organization'")]),
    "uci_items": _map(*ITEM, "input.StockCode", [("id", "input.StockCode"), ("code", "input.StockCode"), ("label", "input.Description ?? ''")]),
    # Lines without a customer (null or empty, as in `reference`) are skipped, so no empty-key Party is created.
    "uci_customers": _map(*PARTY, "input.CustomerID ?? ''", [("display_name", "input.CustomerID ?? ''"), ("kind", "'person'")],
                          when="(input.CustomerID ?? '') != ''"),
}


def positive(value):
    return isinstance(value, (int, float)) and math.isfinite(value) and value > 0


class Budget:
    """Hard cap on estimated spend, priced from the catalogue's per-token prices.

    Each call reserves its worst-case cost under the lock before it is sent: every prompt byte as a
    token plus per-message overhead, and the full completion limit. A reply settles the reservation
    to its reported usage. A refused HTTP request releases it. A call whose outcome is unknown
    (timeout, dropped connection, unreadable or usage-free reply) keeps the worst case. Admission
    requires spent + reserved + worst case <= cap, so estimated spend never exceeds the cap with any
    number of workers. If a reply's reported cost ever exceeds its reservation (for example
    reasoning tokens billed outside the completion limit), the worst case was wrong: the budget
    records the overshoot and admits no further calls.
    """
    def __init__(self, cap, prices):
        if not positive(cap):
            raise ValueError(f"cap must be a finite positive number, got {cap!r}")
        bad = sorted(model for model, pair in prices.items() if not all(positive(price) for price in pair))
        if bad:
            raise ValueError(f"prices must be finite positive numbers; invalid for {bad}")
        self.cap, self.prices, self.spent, self.reserved, self.lock = cap, prices, 0.0, 0.0, threading.Lock()
        self.calls = 0
        self.tokens = collections.Counter()
        self.overshoots = []

    def open(self):
        with self.lock:
            return not self.overshoots and self.spent + self.reserved < self.cap

    def reserve(self, model, messages):
        cin, cout = self.prices[model]
        prompt = sum(len(m["content"].encode()) + 16 for m in messages)
        worst = prompt * cin + MAX_COMPLETION * cout
        with self.lock:
            if self.overshoots or self.spent + self.reserved + worst > self.cap:
                return None
            self.reserved += worst
        return worst

    def release(self, reservation):
        with self.lock:
            self.reserved -= reservation

    def forfeit(self, reservation):
        with self.lock:
            self.reserved -= reservation
            self.spent += reservation
            self.calls += 1
            self.tokens["unknown_usage_calls"] += 1
        return reservation

    def settle(self, reservation, model, prompt, completion):
        cin, cout = self.prices[model]
        cost = prompt * cin + completion * cout
        with self.lock:
            self.reserved -= reservation
            self.spent += cost
            self.calls += 1
            self.tokens["prompt"] += prompt
            self.tokens["completion"] += completion
            if cost > reservation:
                self.overshoots.append({"model": model, "cost": cost, "reservation": reservation,
                                        "prompt_tokens": prompt, "completion_tokens": completion})
        return cost


def ask(url, token, model, messages, budget, log):
    limit = "max_completion_tokens" if model.startswith("openai/") else "max_tokens"
    body = json.dumps({"model": model, "messages": messages, limit: MAX_COMPLETION}).encode()
    request = urllib.request.Request(url + "/chat/completions", data=body, method="POST", headers={
        "Authorization": "Bearer " + token, "Content-Type": "application/json", "User-Agent": "norn-bench"})
    for attempt in range(3):
        reservation = budget.reserve(model, messages)
        if reservation is None:
            return None, "budget"
        try:
            with urllib.request.urlopen(request, timeout=180) as response:
                data = json.load(response)
        except urllib.error.HTTPError as error:
            budget.release(reservation)
            detail = error.read()[:200].decode("utf-8", "replace")
            if error.code < 500 and error.code != 429 or attempt == 2:
                return None, f"http {error.code}: {detail}".replace(token, "***").replace(url, "<gateway>")
            time.sleep(5 * (attempt + 1))
            continue
        except (OSError, ValueError, http.client.HTTPException) as error:
            # The gateway may have billed a call whose reply was lost, so the worst case stays charged.
            cost = budget.forfeit(reservation)
            log({"model": model, "failure": type(error).__name__, "cost": cost, "spent": budget.spent})
            if attempt == 2:
                return None, type(error).__name__
            time.sleep(5 * (attempt + 1))
            continue
        usage = data.get("usage") if isinstance(data, dict) else None
        usage = usage if isinstance(usage, dict) else {}
        prompt, completion = usage.get("prompt_tokens"), usage.get("completion_tokens")
        if isinstance(prompt, int) and isinstance(completion, int):
            cost = budget.settle(reservation, model, prompt, completion)
        else:
            prompt = completion = None
            cost = budget.forfeit(reservation)
        log({"model": model, "prompt_tokens": prompt, "completion_tokens": completion, "cost": cost, "spent": budget.spent})
        try:
            return (data["choices"][0]["message"].get("content") or ""), None
        except (KeyError, IndexError, TypeError, AttributeError):
            return None, "malformed_response"
    return None, "retries exhausted"


def prompt_for(task, rows):
    fields = [{"name": n, "type": t, "required": r} for n, t, r in task["fields"]]
    return (f"Task: {task['brief']}\n\nTarget contract: {json.dumps(task['contract'])}\n\nSource dataset Rows, fields: "
            f"{json.dumps(fields)}\n\nSample rows: {json.dumps(rows[:5])}")


def candidate(task, rows, maps):
    return {"entity": task["entity"], "maps": maps, "rows": rows,
            "fields": [{"name": n, "type": t, "required": r} for n, t, r in task["fields"]]}


def parse(text):
    match = re.search(r"\{.*\}", text or "", re.S)
    if not match:
        raise ValueError("no JSON object in reply")
    maps = json.loads(match.group(0)).get("maps")
    if not isinstance(maps, list) or not maps:
        raise ValueError("reply has no maps array")
    return maps


def row_problems(result):
    return [row for row in result["rows"] if row.get("capture_error") or row.get("process_error") or row.get("diagnostics")]


def reference(name, row):
    """Expected key and properties for one source row, or None when no object should exist."""
    name = name.removesuffix("_blind")
    if name == "olist_products":
        return row["product_id"], {"id": row["product_id"], "code": row["product_id"], "label": row.get("product_category_name")}
    if name == "olist_customers":
        return row["customer_unique_id"], {"kind": "person"}
    if name == "olist_sellers":
        return row["seller_id"], {"kind": "organization"}
    if name == "uci_items":
        return row["StockCode"], {"id": row["StockCode"], "code": row["StockCode"], "label": row.get("Description")}
    if row.get("CustomerID") in (None, ""):
        return None
    return str(row["CustomerID"]), {"kind": "person"}


def score(name, rows, result):
    correct = 0
    for source, outcome in zip(rows, result["rows"]):
        expected = reference(name, source)
        records = outcome.get("records") or []
        if expected is None:
            correct += not records
            continue
        if len(records) != 1 or records[0].get("key") != expected[0]:
            continue
        values = (records[0].get("invocations") or [{}])[0].get("input") or {}
        correct += all(want is None or values.get(prop) == want for prop, want in expected[1].items())
    return correct, len(rows)


def trial(url, token, model, name, repeat, data, check, budget, log):
    task = TASKS[name]
    rows = data[task["rows"]]
    messages = [{"role": "system", "content": GUIDE}, {"role": "user", "content": prompt_for(task, rows)}]
    outcome = {"model": model, "task": name, "repeat": repeat, "rounds": 0, "admitted": False, "clean": False,
               "refusals": [], "correct": 0, "total": len(rows)}
    for _ in range(MAX_ROUNDS):
        if not budget.open():
            outcome["stopped"] = "budget"
            return outcome
        text, failure = ask(url, token, model, messages, budget, log)
        if failure:
            outcome["stopped"] = failure
            return outcome
        outcome["rounds"] += 1
        messages.append({"role": "assistant", "content": text or ""})
        try:
            maps = parse(text)
        except ValueError as error:
            outcome["refusals"].append("harness.invalid_reply")
            messages.append({"role": "user", "content": f"Your reply was not usable: {error}. Return only the JSON object."})
            continue
        result = check(candidate(task, rows, maps))
        if not result["admitted"]:
            outcome["refusals"].extend(d["code"] for d in result["diagnostics"])
            feedback = "\n".join(f"{d['code']}: {d['message']}" for d in result["diagnostics"][:8])
            messages.append({"role": "user", "content": "The checker refused this mapping:\n" + feedback + "\nReturn the corrected JSON object."})
            continue
        outcome["admitted"] = True
        outcome["correct"], outcome["total"] = score(name, rows, result)
        problems = [p for p in row_problems(result) if reference(name, rows[p["row"]]) is not None]
        if not problems:
            outcome["clean"] = True
            outcome["maps"] = maps
            return outcome
        outcome["refusals"].append("runtime.row_failure")
        sample = json.dumps(problems[0])[:600]
        messages.append({"role": "user", "content": f"The mapping was admitted, but {len(problems)} of {len(rows)} sample rows failed. First failure: {sample}\nReturn the corrected JSON object."})
    return outcome


def traps(data, check):
    results = []
    for name, kind, task_name, maps in TRAPS:
        task = TASKS[task_name]
        rows = data[task["rows"]]
        result = check(candidate(task, rows, maps))
        entry = {"trap": name, "kind": kind, "admitted": result["admitted"],
                 "codes": sorted({d["code"] for d in result.get("diagnostics", [])})}
        if result["admitted"]:
            entry["correct"], entry["total"] = score(task_name, rows, result)
            entry["row_failures"] = len(row_problems(result))
        results.append(entry)
    return results


def references(data, check):
    results = {}
    for name, maps in REFERENCE.items():
        task = TASKS[name]
        rows = data[task["rows"]]
        result = check(candidate(task, rows, maps))
        entry = {"admitted": result["admitted"], "diagnostics": result.get("diagnostics", [])}
        if result["admitted"]:
            entry["correct"], entry["total"] = score(name, rows, result)
            entry["row_failures"] = len(row_problems(result))
            entry["sample"] = next((r for r in result["rows"] if r.get("records")), None)
        results[name] = entry
    return results


def run(url, token, models, repeats, data, check, prices, cap, log, workers=5, tasks=None):
    """Run every trial under one shared hard cap (see `Budget`)."""
    unpriced = sorted(set(models) - set(prices))
    if unpriced:
        raise ValueError(f"no catalogue price for {unpriced}; refusing before the first paid call")
    budget = Budget(cap, prices)
    jobs = [(m, t, r) for r in range(repeats) for t in (tasks or TASKS) for m in models]
    with concurrent.futures.ThreadPoolExecutor(workers) as pool:
        outcomes = list(pool.map(lambda j: trial(url, token, j[0], j[1], j[2], data, check, budget, log), jobs))
    return outcomes, budget


def load_prices(path):
    """Per-token (input, output) USD prices by gateway model id."""
    catalogue = json.loads(Path(path).read_text())
    prices = {model: (float(p["input"]), float(p["output"])) for model, p in catalogue["models"].items()}
    bad = sorted(model for model, pair in prices.items() if not all(positive(price) for price in pair))
    if bad:
        raise ValueError(f"prices must be finite positive numbers; invalid for {bad}")
    return prices


def cargo_transport(binary, scratch):
    """The checker transport: one run of the ignored `bench_mapping` test binary per candidate."""
    scratch = Path(scratch)
    scratch.mkdir(parents=True, exist_ok=True)
    counter, lock = itertools.count(), threading.Lock()

    def check(candidate):
        with lock:
            path = scratch / f"candidate-{next(counter)}.json"
        path.write_text(json.dumps(candidate))
        env = {k: v for k, v in os.environ.items() if not k.startswith("NORN_BENCH_GATEWAY_")}
        env["NORN_BENCH_CANDIDATE"] = str(path)
        try:
            done = subprocess.run([binary, "candidate_mapping_admission_and_rows", "--exact", "--ignored", "--nocapture"],
                                  capture_output=True, text=True, env=env, timeout=600, check=False)
        except subprocess.TimeoutExpired:
            return {"admitted": False, "diagnostics": [{"code": "harness.transport", "message": "checker timed out"}]}
        for line in done.stdout.splitlines():
            if line.startswith("BENCH-MAP "):
                return json.loads(line.removeprefix("BENCH-MAP "))
        return {"admitted": False, "diagnostics": [{"code": "harness.transport", "message": done.stderr[-400:]}]}
    return check


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", required=True, help="bench_mapping test binary")
    parser.add_argument("--rows", required=True, help="JSON object of source rows by task rows name")
    parser.add_argument("--scratch", required=True, help="directory for candidates and the transcript")
    parser.add_argument("--prices", default="fixtures/bench/mapping-trial-prices.json")
    parser.add_argument("--models", required=True, help="comma-separated gateway model ids")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--cap", type=float, required=True, help="hard cap on estimated USD spend")
    parser.add_argument("--workers", type=int, default=5)
    parser.add_argument("--tasks", help="comma-separated task names; default all")
    args = parser.parse_args()
    url, token = os.environ["NORN_BENCH_GATEWAY_URL"].rstrip("/"), os.environ["NORN_BENCH_GATEWAY_TOKEN"]
    data = json.loads(Path(args.rows).read_text())
    check = cargo_transport(args.binary, Path(args.scratch) / "candidates")
    transcript, lock = (Path(args.scratch) / "calls.jsonl").open("a"), threading.Lock()

    def log(entry):
        with lock:
            transcript.write(json.dumps(entry) + "\n")
            transcript.flush()
    print(json.dumps({"references": references(data, check), "traps": traps(data, check)}))
    outcomes, budget = run(url, token, args.models.split(","), args.repeats, data, check, load_prices(args.prices),
                           args.cap, log, workers=args.workers, tasks=args.tasks.split(",") if args.tasks else None)
    print(json.dumps({"outcomes": outcomes, "calls": budget.calls, "tokens": budget.tokens, "spent": budget.spent,
                      "overshoots": budget.overshoots}))


if __name__ == "__main__":
    main()
