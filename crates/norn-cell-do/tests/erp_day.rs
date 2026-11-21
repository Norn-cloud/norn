//! ERP-DAY-1: how far one cell goes under an ERP day, replayed as commands.
//!
//! The real `norn-dev-host` serves the ERP model in `fixtures/erp-day` (its
//! TypeScript compiled by the real worker, admitted and installed by the
//! host), and N client threads drive it over HTTP/1.1 keep-alive exactly as
//! Studio does: every write is `POST /v1/actions/{id}` through the full
//! action path (access policy, preconditions over the read slice, laws,
//! approval, idempotency, receipt, ledger, export journal). One sales cycle
//! is about 30 commands: customer (sometimes), sales order and lines, submit,
//! delivery and one stock move per line, invoice and lines, a balanced
//! journal entry, payment and allocation, a second journal entry, and every
//! fourth cycle a purchase order (over 5000 parks for the reviewer's
//! sign-off), stock receipts and its journal entry. One cycle in 50 also
//! tries to over-issue stock, which the law must refuse.
//!
//! `#[ignore]`d; run in release on a quiet machine with `bun` on `PATH` and
//! `tools/norn-ts` installed:
//! `cargo test --release -p norn-cell-do --test erp_day -- --ignored --nocapture --test-threads 1`
//!
//! - `NORN_ERP_CLIENTS` (default `1,10,100`): the concurrency levels, run in
//!   order on one cell; `NORN_ERP_SECONDS` (default 30) per level.
//! - `NORN_ERP_COMMANDS`: instead of timed levels, run `NORN_ERP_CLIENTS`'
//!   first level until this many commands committed (the storage run).
//! - `NORN_ERP_DIR`: the project directory to use and keep (default: a new
//!   temporary one, removed). Reusing a kept directory reopens its cell.
//! - `NORN_ERP_REOPEN=1`: only open the kept cell, then time the first
//!   command and one cycle (the cell-size operations run).
//! - `NORN_ERP_BUN` (default `bun`): the runtime for the compile worker.
//! - `NORN_ERP_RUN`: a fixed run prefix for the keys, and `NORN_ERP_OUTCOMES`:
//!   a file for every command's outcome (ERP-EQUIV-1). With one client and
//!   `NORN_ERP_COMMANDS`, two builds given the same prefix run the same
//!   commands in the same order, so their outcome files must be identical.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    clippy::too_many_lines,
    clippy::disallowed_methods,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_wrap,
    clippy::panic,
    clippy::string_slice,
    clippy::needless_pass_by_value,
    clippy::wildcard_enum_match_arm,
    clippy::semicolon_if_nothing_returned,
    clippy::manual_is_multiple_of,
    reason = "a bench: wall clock, printed tables, fixed fixtures"
)]

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const SCOPE: &str = "orgId=org_erp&environmentId=dev";
const CUSTOMERS: usize = 50;
const ITEMS: usize = 200;
const OPENING_STOCK: i64 = 1_000_000_000;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// --- The dev host ----------------------------------------------------------

struct Host {
    child: Child,
    addr: String,
    developer: String,
    reviewer: String,
    dir: PathBuf,
    keep: bool,
    _events: Mutex<Receiver<Value>>,
}

impl Host {
    /// Start `norn-dev-host` over `dir` and wait for `ready`; returns the
    /// host and the seconds from spawn to `ready`.
    fn start(dir: PathBuf, keep: bool) -> (Self, f64) {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/erp-day");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::copy(fixture.join("norn.json"), dir.join("norn.json")).unwrap();
        std::fs::copy(fixture.join("src/erp.ts"), dir.join("src/erp.ts")).unwrap();
        let worker = repo_root().join("tools/norn-ts/src/worker.ts");
        let started = Instant::now();
        let mut child = Command::new(env!("CARGO_BIN_EXE_norn-dev-host"))
            .arg("--project")
            .arg(&dir)
            .args(["--json", "--port", "0", "--"])
            .arg(env_or("NORN_ERP_BUN", "bun"))
            .arg(&worker)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn norn-dev-host");
        let stdout = child.stdout.take().unwrap();
        let (send, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(event) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if event["event"] != "ready" && event["event"] != "loaded" {
                    eprintln!("dev host: {line}");
                }
                if send.send(event).is_err() {
                    break;
                }
            }
        });
        let ready = loop {
            let event = events
                .recv_timeout(Duration::from_secs(600))
                .expect("norn-dev-host became ready");
            assert_ne!(event["event"], "fatal", "{event}");
            assert_ne!(
                event["event"], "rejected",
                "the ERP model was refused: {event}"
            );
            if event["event"] == "ready" {
                break event;
            }
        };
        let open_s = started.elapsed().as_secs_f64();
        let url = ready["url"].as_str().unwrap();
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(".norn").join(name))
                .unwrap()
                .trim()
                .to_owned()
        };
        let host = Self {
            child,
            addr: url.trim_start_matches("http://").to_owned(),
            developer: read("dev.token"),
            reviewer: read("dev.reviewer.token"),
            dir,
            keep,
            _events: Mutex::new(events),
        };
        (host, open_s)
    }

    fn cell(&self) -> PathBuf {
        self.dir.join(".norn/cell.sqlite")
    }

    fn cell_bytes(&self) -> u64 {
        ["", "-wal", "-shm", "-journal"]
            .iter()
            .filter_map(|suffix| {
                std::fs::metadata(format!("{}{suffix}", self.cell().display())).ok()
            })
            .map(|meta| meta.len())
            .sum()
    }

    /// The host process's CPU time so far (user + system), in seconds, from
    /// `/proc` (Linux; 0 elsewhere). Clock ticks are 100 per second on the
    /// Linux targets this bench runs on.
    fn cpu_seconds(&self) -> f64 {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", self.child.id())) else {
            return 0.0;
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            return 0.0;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // After the command name: state is field 3, utime 14, stime 15.
        let ticks = |index: usize| fields.get(index - 3).and_then(|f| f.parse::<f64>().ok()).unwrap_or(0.0);
        (ticks(14) + ticks(15)) / 100.0
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _killed = self.child.kill();
        let _waited = self.child.wait();
        if !self.keep {
            let _removed = std::fs::remove_dir_all(&self.dir);
        }
    }
}

// --- A keep-alive HTTP/1.1 client ------------------------------------------

struct Conn {
    addr: String,
    stream: Option<BufReader<TcpStream>>,
    reconnects: u64,
}

impl Conn {
    fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_owned(),
            stream: None,
            reconnects: 0,
        }
    }

    fn call(
        &mut self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<&Value>,
    ) -> (u16, Value) {
        let body = body.map(Value::to_string).unwrap_or_default();
        let separator = if path.contains('?') { '&' } else { '?' };
        let request = format!(
            "{method} {path}{separator}{SCOPE} HTTP/1.1\r\nhost: bench\r\nauthorization: Bearer {token}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        for attempt in 0..3 {
            let reused = self.stream.is_some();
            if self.stream.is_none() {
                let stream = TcpStream::connect(&self.addr).expect("connect");
                stream.set_nodelay(true).unwrap();
                self.stream = Some(BufReader::new(stream));
                self.reconnects += 1;
            }
            match self.exchange(&request) {
                Ok(answer) => return answer,
                // A pooled connection the host closed: open a new one.
                Err(error) if reused || attempt < 2 => {
                    self.stream = None;
                    if !reused {
                        eprintln!("retrying after {error}");
                    }
                }
                Err(error) => panic!("{method} {path}: {error}"),
            }
        }
        panic!("{method} {path}: no answer")
    }

    fn exchange(&mut self, request: &str) -> std::io::Result<(u16, Value)> {
        let reader = self.stream.as_mut().unwrap();
        reader.get_mut().write_all(request.as_bytes())?;
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::other("closed before the status line"));
        }
        let status: u16 = line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| std::io::Error::other(format!("bad status line {line:?}")))?;
        let (mut length, mut close) = (0_usize, false);
        loop {
            line.clear();
            reader.read_line(&mut line)?;
            let header = line.trim_end();
            if header.is_empty() {
                break;
            }
            let (name, value) = header.split_once(':').unwrap_or((header, ""));
            match name.to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap_or(0),
                "connection" => close = value.trim().eq_ignore_ascii_case("close"),
                _ => {}
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        if close {
            self.stream = None;
        }
        let value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }
}

// --- Measurements -----------------------------------------------------------

#[derive(Default)]
struct Tally {
    /// Commit latency of every committed command, microseconds, by action.
    latency: BTreeMap<&'static str, Vec<u64>>,
    committed: u64,
    reads: u64,
    /// Calls the cell refused as the workload expects (over-issue).
    expected_refusals: u64,
    /// Stock moves re-read and retried after a concurrent move changed the bin.
    stale_retries: u64,
    /// Reads repeated after a retryable refusal (`object.snapshot_moved`:
    /// a commit landed while the read was assembled).
    read_retries: u64,
    parked: u64,
    unexpected: Vec<String>,
    /// ERP-EQUIV-1: every command's outcome in call order (action, key,
    /// status, refusal code, answer shape), for comparing two builds.
    outcomes: Vec<String>,
}

impl Tally {
    fn merge(&mut self, other: Self) {
        for (action, mut values) in other.latency {
            self.latency.entry(action).or_default().append(&mut values);
        }
        self.committed += other.committed;
        self.reads += other.reads;
        self.expected_refusals += other.expected_refusals;
        self.stale_retries += other.stale_retries;
        self.read_retries += other.read_retries;
        self.parked += other.parked;
        self.unexpected.extend(other.unexpected);
        self.outcomes.extend(other.outcomes);
    }

    fn all(&self) -> Vec<u64> {
        let mut all: Vec<u64> = self.latency.values().flatten().copied().collect();
        all.sort_unstable();
        all
    }
}

/// ERP-EQUIV-1: what a command's answer says independent of the wall clock:
/// its status, its refusal code, and its sorted top-level keys with every
/// integer member (sequences and versions).
fn outcome(action: &str, key: &str, status: u16, body: &Value) -> String {
    let code = body.get("code").and_then(Value::as_str).unwrap_or("");
    let shape: Vec<String> = body.as_object().map_or_else(Vec::new, |object| {
        let mut keys: Vec<String> = object
            .iter()
            .map(|(name, value)| match value {
                Value::Number(number) if number.is_u64() => format!("{name}={number}"),
                _ => name.clone(),
            })
            .collect();
        keys.sort();
        keys
    });
    format!("{action}\t{key}\t{status}\t{code}\t{}", shape.join(","))
}

fn percentile(sorted: &[u64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[rank.min(sorted.len() - 1)] as f64 / 1000.0
}

// --- The workload ------------------------------------------------------------

fn money(cents: i64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

/// An exact decimal the host answered (text or number), in hundredths.
fn cents(value: &Value) -> i64 {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let fraction = format!("{fraction:0<2}");
    let negative = whole.starts_with('-');
    let magnitude = whole.trim_start_matches('-').parse::<i64>().unwrap() * 100
        + fraction[..2].parse::<i64>().unwrap();
    if negative { -magnitude } else { magnitude }
}

/// A deterministic generator per client.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct Client<'a> {
    conn: Conn,
    host: &'a Host,
    tally: Tally,
    rng: Rng,
    prefix: String,
}

fn rate_cents(item: usize) -> i64 {
    500 + (item as i64 * 37) % 9_500
}

impl Client<'_> {
    /// One command; counts its latency when it commits. `Err` carries the
    /// refusal for the caller to judge.
    fn act(
        &mut self,
        action: &'static str,
        key: &str,
        input: Value,
    ) -> Result<Value, (u16, Value)> {
        let started = Instant::now();
        let token = self.host.developer.clone();
        let (status, body) = self.conn.call(
            "POST",
            &format!("/v1/actions/erp.{action}"),
            &token,
            Some(&json!({ "input": input, "idempotencyKey": key })),
        );
        let micros = started.elapsed().as_micros() as u64;
        self.tally.outcomes.push(outcome(action, key, status, &body));
        if status == 200 {
            self.tally.latency.entry(action).or_default().push(micros);
            self.tally.committed += 1;
            Ok(body)
        } else {
            Err((status, body))
        }
    }

    fn must(&mut self, action: &'static str, key: &str, input: Value) {
        if let Err((status, body)) = self.act(action, key, input) {
            self.tally
                .unexpected
                .push(format!("{action} {key}: {status} {body}"));
        }
    }

    fn read(&mut self, kind: &str, id: &str) -> Option<Value> {
        self.tally.reads += 1;
        let token = self.host.developer.clone();
        let path = format!("/v1/objects/erp.{kind}/{id}");
        let (mut status, mut body) = self.conn.call("GET", &path, &token, None);
        // A retryable refusal says to repeat the read, as a client does.
        for _ in 0..20 {
            if status == 200 || body["retryable"] != Value::Bool(true) {
                break;
            }
            self.tally.read_retries += 1;
            (status, body) = self.conn.call("GET", &path, &token, None);
        }
        self.tally
            .outcomes
            .push(if status == 200 {
                format!("read\t{kind}/{id}\t{status}\t{}", body["values"])
            } else {
                format!("read\t{kind}/{id}\t{status}\t{body}")
            });
        (status == 200).then_some(body)
    }

    /// Move stock with the balance it leaves, re-reading the bin when a
    /// concurrent move changed it first.
    fn move_stock(&mut self, item: usize, delta: i64, key: &str) {
        let bin = format!("BIN-{item:04}");
        for attempt in 0..50 {
            let Some(current) = self.read("Bin", &bin) else {
                let why = self.tally.outcomes.last().cloned().unwrap_or_default();
                self.tally.unexpected.push(format!("no bin {bin}: {why}"));
                return;
            };
            let on_hand = cents(&current["values"]["on_hand"]) / 100;
            let input = json!({ "bin_id": bin, "on_hand": (on_hand + delta).to_string(), "last_move": delta.to_string() });
            match self.act("stock.move", &format!("{key}-{attempt}"), input) {
                Ok(_) => return,
                Err(_) if attempt < 49 => self.tally.stale_retries += 1,
                Err((status, body)) => self
                    .tally
                    .unexpected
                    .push(format!("stock.move {bin}: {status} {body}")),
            }
        }
    }

    fn journal(&mut self, id: &str, memo: &str, legs: &[(&str, i64, i64)]) {
        self.must(
            "journal.create",
            &format!("{id}-c"),
            json!({ "doc_id": id, "memo": memo }),
        );
        for (n, (account, debit, credit)) in legs.iter().enumerate() {
            self.must(
                "journal.add_line",
                &format!("{id}-l{n}"),
                json!({ "line_id": format!("{id}-{n}"), "doc_id": id, "account": account, "debit": money(*debit), "credit": money(*credit) }),
            );
        }
        self.must("journal.post", &format!("{id}-p"), json!({ "doc_id": id }));
    }

    fn lines(&mut self, action: &'static str, doc: &str, lines: &[(usize, i64)]) {
        for (n, (item, qty)) in lines.iter().enumerate() {
            let rate = rate_cents(*item);
            self.must(
                action,
                &format!("{doc}-l{n}"),
                json!({ "line_id": format!("{doc}-{n}"), "doc_id": doc, "item_code": format!("ITEM-{item:04}"),
                        "qty": qty.to_string(), "rate": money(rate), "amount": money(rate * qty) }),
            );
        }
    }

    /// One sales cycle (and every fourth one a purchase), about 30 commands.
    fn cycle(&mut self, n: u64) {
        let id = format!("{}n{n}", self.prefix);
        let customer = if n % 20 == 0 {
            let customer = format!("CUST-{id}");
            self.must(
                "customer.create",
                &format!("{customer}-c"),
                json!({ "customer_id": customer, "customer_name": format!("Customer {id}"), "credit_limit": money(1_000_000_000) }),
            );
            customer
        } else {
            format!("CUST-{:04}", self.rng.below(CUSTOMERS as u64))
        };
        let count = 1 + self.rng.below(5) as usize;
        let lines: Vec<(usize, i64)> = (0..count)
            .map(|_| {
                (
                    self.rng.below(ITEMS as u64) as usize,
                    1 + self.rng.below(20) as i64,
                )
            })
            .collect();
        let total: i64 = lines
            .iter()
            .map(|(item, qty)| rate_cents(*item) * qty)
            .sum();

        let order = format!("SO-{id}");
        self.must(
            "sales_order.create",
            &format!("{order}-c"),
            json!({ "doc_id": order, "customer_id": customer, "total": money(total) }),
        );
        self.lines("sales_order.add_line", &order, &lines);
        self.must(
            "sales_order.submit",
            &format!("{order}-s"),
            json!({ "doc_id": order }),
        );

        let delivery = format!("DN-{id}");
        self.must(
            "delivery.create",
            &format!("{delivery}-c"),
            json!({ "doc_id": delivery, "order_id": order }),
        );
        for (n, (item, qty)) in lines.iter().enumerate() {
            self.move_stock(*item, -qty, &format!("{delivery}-m{n}"));
        }
        self.must(
            "delivery.submit",
            &format!("{delivery}-s"),
            json!({ "doc_id": delivery }),
        );
        self.must(
            "sales_order.complete",
            &format!("{order}-d"),
            json!({ "doc_id": order }),
        );

        let invoice = format!("SI-{id}");
        self.must(
            "sales_invoice.create",
            &format!("{invoice}-c"),
            json!({ "doc_id": invoice, "customer_id": customer, "order_id": order, "total": money(total), "outstanding": money(total) }),
        );
        self.lines("sales_invoice.add_line", &invoice, &lines);
        self.must(
            "sales_invoice.submit",
            &format!("{invoice}-s"),
            json!({ "doc_id": invoice }),
        );
        let tax = total / 7;
        self.journal(
            &format!("JE-{id}-si"),
            "sales invoice",
            &[
                ("1200 Receivables", total, 0),
                ("4000 Revenue", 0, total - tax),
                ("2300 Tax payable", 0, tax),
            ],
        );

        let payment = format!("PE-{id}");
        self.must("payment.receive", &format!("{payment}-c"), json!({ "doc_id": payment, "customer_id": customer, "amount": money(total), "unallocated": money(total) }));
        self.must(
            "payment.allocate",
            &format!("{payment}-a"),
            json!({ "doc_id": payment, "unallocated": money(0) }),
        );
        self.must(
            "sales_invoice.allocate",
            &format!("{invoice}-a"),
            json!({ "doc_id": invoice, "outstanding": money(0), "last_allocated": money(total) }),
        );
        self.must(
            "sales_invoice.complete",
            &format!("{invoice}-d"),
            json!({ "doc_id": invoice }),
        );
        self.journal(
            &format!("JE-{id}-pe"),
            "payment",
            &[("1000 Bank", total, 0), ("1200 Receivables", 0, total)],
        );

        if n % 50 == 7 {
            // The law refuses a move that would leave the bin negative.
            let bin = format!("BIN-{:04}", lines[0].0);
            if let Some(current) = self.read("Bin", &bin) {
                let on_hand = cents(&current["values"]["on_hand"]) / 100;
                let over = -(on_hand + 1);
                match self.act(
                    "stock.move",
                    &format!("{id}-over"),
                    json!({ "bin_id": bin, "on_hand": "-1", "last_move": over.to_string() }),
                ) {
                    Err((status, _)) if (400..500).contains(&status) => {
                        self.tally.expected_refusals += 1
                    }
                    other => self
                        .tally
                        .unexpected
                        .push(format!("over-issue was not refused: {other:?}")),
                }
            }
        }

        if n % 4 == 1 {
            let po = format!("PO-{id}");
            let lines: Vec<(usize, i64)> = (0..count)
                .map(|_| {
                    (
                        self.rng.below(ITEMS as u64) as usize,
                        10 + self.rng.below(90) as i64,
                    )
                })
                .collect();
            let total: i64 = lines
                .iter()
                .map(|(item, qty)| rate_cents(*item) * qty)
                .sum();
            self.must(
                "purchase_order.create",
                &format!("{po}-c"),
                json!({ "doc_id": po, "supplier": "SUP-0001", "total": money(total) }),
            );
            self.lines("purchase_order.add_line", &po, &lines);
            let submit_key = format!("{po}-s");
            match self.act(
                "purchase_order.submit",
                &submit_key,
                json!({ "doc_id": po }),
            ) {
                Ok(_) => {}
                Err((202, body)) => {
                    // APPROVAL-WHEN-1: over 5000 the reviewer signs off, then
                    // the requester's own retry commits it.
                    self.tally.parked += 1;
                    let decision = body["decision"]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    let token = self.host.reviewer.clone();
                    let (status, answer) = self.conn.call(
                        "POST",
                        &format!("/v1/decisions/{decision}:decide"),
                        &token,
                        Some(
                            &json!({ "choice": "approve", "idempotencyKey": format!("{po}-sign") }),
                        ),
                    );
                    if status != 200 {
                        self.tally
                            .unexpected
                            .push(format!("decide {po}: {status} {answer}"));
                    }
                    self.must(
                        "purchase_order.submit",
                        &submit_key,
                        json!({ "doc_id": po }),
                    );
                }
                Err((status, body)) => self
                    .tally
                    .unexpected
                    .push(format!("purchase_order.submit {po}: {status} {body}")),
            }
            for (n, (item, qty)) in lines.iter().enumerate() {
                self.move_stock(*item, *qty, &format!("{po}-r{n}"));
            }
            self.must(
                "purchase_order.complete",
                &format!("{po}-d"),
                json!({ "doc_id": po }),
            );
            self.journal(
                &format!("JE-{id}-po"),
                "purchase receipt",
                &[("1400 Inventory", total, 0), ("2000 Payables", 0, total)],
            );
        }
    }
}

fn nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", nanos % (1 << 40))
}

/// Master data, once per cell: customers, items and their stock bins.
fn setup(host: &Host) {
    let mut client = Client {
        conn: Conn::new(&host.addr),
        host,
        tally: Tally::default(),
        rng: Rng(1),
        prefix: String::new(),
    };
    if client.read("Customer", "CUST-0000").is_some() {
        return;
    }
    for c in 0..CUSTOMERS {
        let id = format!("CUST-{c:04}");
        client.must("customer.create", &format!("{id}-c"), json!({ "customer_id": id, "customer_name": format!("Customer {c}"), "credit_limit": money(1_000_000_000_000) }));
    }
    for i in 0..ITEMS {
        let item = format!("ITEM-{i:04}");
        client.must("item.create", &format!("{item}-c"), json!({ "item_code": item, "item_name": format!("Item {i}"), "standard_rate": money(rate_cents(i)) }));
        let bin = format!("BIN-{i:04}");
        client.must("bin.open", &format!("{bin}-c"), json!({ "bin_id": bin, "item_code": item, "warehouse": "Stores", "on_hand": OPENING_STOCK.to_string(), "last_move": "0" }));
    }
    assert!(
        client.tally.unexpected.is_empty(),
        "{:#?}",
        client.tally.unexpected
    );
}

/// Run `clients` threads until `deadline` passes or `target` commands commit.
fn level(host: &Host, clients: usize, seconds: f64, target: Option<u64>) -> (Tally, f64, u64) {
    let committed = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let run = std::env::var("NORN_ERP_RUN").unwrap_or_else(|_| nonce());
    let started = Instant::now();
    let deadline = started + Duration::from_secs_f64(seconds);
    let tallies = Mutex::new(Vec::new());
    let reconnects = AtomicU64::new(0);
    std::thread::scope(|scope| {
        for c in 0..clients {
            let (committed, stop, tallies, run, reconnects) =
                (&committed, &stop, &tallies, &run, &reconnects);
            scope.spawn(move || {
                let mut client = Client {
                    conn: Conn::new(&host.addr),
                    host,
                    tally: Tally::default(),
                    rng: Rng(0x9E37_79B9_7F4A_7C15 ^ (c as u64 + 1)),
                    prefix: format!("{run}c{c}"),
                };
                let mut n = 0;
                while !stop.load(Ordering::Relaxed) {
                    let before = client.tally.committed;
                    client.cycle(n);
                    n += 1;
                    let total = committed
                        .fetch_add(client.tally.committed - before, Ordering::Relaxed)
                        + client.tally.committed
                        - before;
                    if target.is_some_and(|target| total >= target)
                        || (target.is_none() && Instant::now() >= deadline)
                    {
                        stop.store(true, Ordering::Relaxed);
                    }
                }
                reconnects.fetch_add(client.conn.reconnects, Ordering::Relaxed);
                tallies.lock().unwrap().push(client.tally);
            });
        }
    });
    let elapsed = started.elapsed().as_secs_f64();
    let mut tally = Tally::default();
    for one in tallies.into_inner().unwrap() {
        tally.merge(one);
    }
    (tally, elapsed, reconnects.into_inner())
}

/// ERP-DAY-1: the host's CPU per committed command and its utilisation.
fn cpu(tally: &Tally, elapsed: f64, cpu: f64) {
    println!(
        "  host cpu {cpu:.1}s = {:.2} ms/command, {:.0}% of one core",
        cpu * 1000.0 / tally.committed.max(1) as f64,
        cpu * 100.0 / elapsed
    );
}

fn report(label: &str, tally: &Tally, elapsed: f64, reconnects: u64, host: &Host) {
    let all = tally.all();
    println!(
        "ERP {label}: {} commands in {elapsed:.1}s = {:.1} cmd/s ({:.1} reads/s) | commit p50 {:.2} ms p95 {:.2} ms p99 {:.2} ms max {:.2} ms | stale retries {} read retries {} parked {} refused-as-expected {} unexpected {} reconnects {} | cell {:.1} MB",
        tally.committed,
        tally.committed as f64 / elapsed,
        tally.reads as f64 / elapsed,
        percentile(&all, 50.0),
        percentile(&all, 95.0),
        percentile(&all, 99.0),
        percentile(&all, 100.0),
        tally.stale_retries,
        tally.read_retries,
        tally.parked,
        tally.expected_refusals,
        tally.unexpected.len(),
        reconnects,
        host.cell_bytes() as f64 / 1e6,
    );
    for (action, values) in &tally.latency {
        let mut sorted = values.clone();
        sorted.sort_unstable();
        println!(
            "  {action:<26} n={:<7} p50 {:>7.2} ms p95 {:>7.2} ms p99 {:>7.2} ms",
            sorted.len(),
            percentile(&sorted, 50.0),
            percentile(&sorted, 95.0),
            percentile(&sorted, 99.0)
        );
    }
    for line in tally.unexpected.iter().take(5) {
        println!(
            "  unexpected: {}",
            line.chars().take(400).collect::<String>()
        );
    }
}

#[test]
#[ignore = "a release bench over the real dev host"]
fn erp_day() {
    let (dir, keep) = std::env::var("NORN_ERP_DIR").map_or_else(
        |_| {
            (
                std::env::temp_dir().join(format!("norn-erp-day-{}", nonce())),
                false,
            )
        },
        |dir| (PathBuf::from(dir), true),
    );
    let (host, open_s) = Host::start(dir, keep);
    println!(
        "ERP open: ready in {open_s:.2}s, cell {:.1} MB",
        host.cell_bytes() as f64 / 1e6
    );

    if std::env::var("NORN_ERP_REOPEN").is_ok() {
        let mut client = Client {
            conn: Conn::new(&host.addr),
            host: &host,
            tally: Tally::default(),
            rng: Rng(7),
            prefix: format!("{}re", nonce()),
        };
        let started = Instant::now();
        let id = format!("CUST-{}-first", client.prefix);
        client.must("customer.create", &format!("{id}-c"), json!({ "customer_id": id, "customer_name": "First after open", "credit_limit": money(100) }));
        let first_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        client.cycle(1);
        let cycle_ms = started.elapsed().as_secs_f64() * 1000.0;
        println!(
            "ERP reopen: first command {first_ms:.1} ms, then one cycle of {} commands in {cycle_ms:.1} ms, unexpected {}",
            client.tally.committed - 1,
            client.tally.unexpected.len()
        );
        assert!(
            client.tally.unexpected.is_empty(),
            "{:#?}",
            client.tally.unexpected
        );
        return;
    }

    let started = Instant::now();
    setup(&host);
    println!(
        "ERP setup: {:.1}s, cell {:.1} MB",
        started.elapsed().as_secs_f64(),
        host.cell_bytes() as f64 / 1e6
    );
    let levels: Vec<usize> = env_or("NORN_ERP_CLIENTS", "1,10,100")
        .split(',')
        .map(|level| level.trim().parse().unwrap())
        .collect();
    let seconds: f64 = env_or("NORN_ERP_SECONDS", "30").parse().unwrap();
    let target = std::env::var("NORN_ERP_COMMANDS")
        .ok()
        .map(|value| value.parse::<u64>().unwrap());
    let mut unexpected = 0;
    if let Some(target) = target {
        let before = host.cell_bytes();
        let cpu_before = host.cpu_seconds();
        let (tally, elapsed, reconnects) = level(&host, levels[0], 1e9, Some(target));
        cpu(&tally, elapsed, host.cpu_seconds() - cpu_before);
        report(
            &format!("{} clients, storage run", levels[0]),
            &tally,
            elapsed,
            reconnects,
            &host,
        );
        write_outcomes(&tally);
        let grown = host.cell_bytes().saturating_sub(before);
        println!(
            "ERP storage: {} commands grew the cell {:.1} MB = {:.0} bytes/command",
            tally.committed,
            grown as f64 / 1e6,
            grown as f64 / tally.committed as f64
        );
        unexpected += tally.unexpected.len();
    } else {
        for clients in levels {
            let cpu_before = host.cpu_seconds();
            let (tally, elapsed, reconnects) = level(&host, clients, seconds, None);
            cpu(&tally, elapsed, host.cpu_seconds() - cpu_before);
            report(
                &format!("{clients} clients"),
                &tally,
                elapsed,
                reconnects,
                &host,
            );
            unexpected += tally.unexpected.len();
        }
    }
    assert_eq!(unexpected, 0, "unexpected refusals; see above");
}

/// ERP-EQUIV-1: with `NORN_ERP_OUTCOMES` set, write every command's outcome.
fn write_outcomes(tally: &Tally) {
    if let Ok(path) = std::env::var("NORN_ERP_OUTCOMES") {
        let mut text = tally.outcomes.join("\n");
        text.push('\n');
        std::fs::write(path, text).unwrap();
    }
}
