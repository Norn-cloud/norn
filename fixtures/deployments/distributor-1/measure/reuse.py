#!/usr/bin/env python3
"""The deployment-to-deployment cost metric: how much of distributor-1's
ontology and mapping came from existing packs, and how much was written new.

Counted four ways, because one number hides the cost:
  artifacts   compiled artifacts the deployment admits, by origin
  maps        object maps: copied verbatim from a pack vs authored
  fields      declared source fields: in a re-bound pack schema vs new
  lines       authored TypeScript/spec lines vs lines reused unchanged
and a projection for deployment #2: each new artifact is classed
system-generic (any ERPNext/Odoo/Saleor/MT940/camt/X12 customer needs it; a
pack candidate) or deployment-specific (this distributor's regions, crosswalk,
types). Promote the generic ones and #2 starts from that reuse.

Usage: reuse.py [OUT_JSON]   (compiles with bun; run from the repo checkout)
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parents[1]
REPO = HERE.parents[2]
GEN = HERE / "project/src/gen"

GENERIC = {  # new artifacts any deployment of that system would need
    "d1.erpnext_docs.source": "erpnext", "d1.erpnext_docs.mapping": "erpnext", "d1.odoo_docs.source": "odoo", "d1.odoo_docs.mapping": "odoo",
    "d1.saleor_docs.source": "saleor", "d1.saleor_docs.mapping": "saleor", "d1.files.source": "bank/edi files", "d1.files.mapping": "bank/edi files",
    "d1.bpic.source": "sap-p2p", "d1.bpic.mapping": "sap-p2p", "d1.erpnext.source": "erpnext", "d1.odoo.source": "odoo", "d1.saleor.source": "saleor",
    "distributor.SalesInvoice.entity": "canon gap: invoice", "distributor.InvoiceLine.entity": "canon gap: invoice", "distributor.CustomerOrderLine.entity": "canon gap: order document", "distributor.CustomerOrder.entity": "canon gap: order document",
    "distributor.BankReceipt.entity": "canon gap: bank line", "distributor.Reconciliation.entity": "canon gap: settlement matching", "distributor.Reconciliation.pays_its_invoice.law": "canon gap: settlement matching",
    "distributor.receipt.reconcile.action": "canon gap: settlement matching", "distributor.StockLedger.entity": "canon gap: stock ledger",
    "distributor.StockMove.entity": "canon gap: stock ledger", "distributor.StockLedger.conserves.law": "canon gap: stock ledger",
    "distributor.PriceListEntry.entity": "canon gap: price list", "distributor.ReceivableBalance.entity": "canon gap: receivables",
    "distributor.CustomerProfile.entity": "canon gap: party attributes", "distributor.Observed.interface": "mapping needs an interface",
    "p2p.Case.entity": "canon gap: p2p", "p2p.Event.entity": "canon gap: p2p",
    # pack-track mappings: maps verbatim, re-issued only because a mapping pins its source schema's hash
    "d1.erpnext.mapping": "pack rebinding (source pin)", "d1.odoo.mapping": "pack rebinding (source pin)", "d1.saleor.mapping": "pack rebinding (source pin)",
}


def compile_module(path: Path) -> list[dict]:
    r = subprocess.run(["bun", str(REPO / "tools/norn-ts/src/cli.ts"), "compile", str(path), "--org", "org_d1", "--env", "dev"], capture_output=True, text=True, timeout=180, cwd=REPO)
    try:
        doc = json.loads(r.stdout)
    except ValueError:
        return []
    return doc if isinstance(doc, list) else []


def main() -> None:
    subprocess.run([sys.executable, "-I", str(HERE / "project/gen_mappings.py"), str(GEN)], check=True, capture_output=True)
    template = json.loads((REPO / "tools/norn-ts/templates/three-systems/template.json").read_text())
    reused = [f"core:{c}" for c in template["core"]]
    for pack in ("canon", "locale-ar"):
        reused += [f"{pack}:{p.stem}" for p in sorted((REPO / "registry/packs" / pack / "artifacts").glob("*.json"))]
    new, authored_lines = [], 0
    for mod in sorted((HERE / "project/src").glob("*.ts")) + sorted(GEN.glob("*.ts")):
        arts = compile_module(mod)
        new += [{"id": a["artifactId"], "kind": a["kind"], "module": mod.name} for a in arts]
        if mod.parent.name == "src":
            authored_lines += sum(1 for l in mod.read_text().splitlines() if l.strip() and not l.strip().startswith("//"))
    cov = json.loads((GEN / "coverage.json").read_text())
    pack_maps = sum(v["maps"] for v in cov.values() if v["track"] == "pack")
    new_maps = sum(v["maps"] for v in cov.values() if v["track"] == "new")
    pack_fields = sum(v["fields"] for v in cov.values() if v["track"] == "pack")
    new_fields = sum(v["fields"] for v in cov.values() if v["track"] == "new")
    pack_map_lines = sum(v["mapLines"] for v in cov.values() if v["track"] == "pack")
    spec = (HERE / "project/gen_mappings.py").read_text()
    spec_lines = sum(1 for l in spec[spec.index("SPEC: dict"):spec.index("IFACE = {")].splitlines() if l.strip() and not l.strip().startswith("#"))
    rebinding_lines = sum(1 for l in spec[spec.index("PACK_REBIND = {"):spec.index("def _ts")].splitlines() if l.strip())
    generic = [a for a in new if a["id"] in GENERIC]
    specific = [a for a in new if a["id"] not in GENERIC]
    total_art = len(reused) + len(new)
    # canon types that mappings target vs deployment types
    targets = re.findall(r"target: \{type: (\w+),", "".join(p.read_text() for p in GEN.glob("*.ts")))
    canon_targets = sum(1 for t in targets if t in ("Party", "Item", "Holding", "OrderLine", "Delivery", "Posting", "SettlementObservation", "PartyRole"))
    report = {
        "artifacts": {"reused": len(reused), "new": len(new), "reusePct": round(100 * len(reused) / total_art, 1)},
        "maps": {"copiedVerbatimFromPacks": pack_maps, "authored": new_maps, "reusePct": round(100 * pack_maps / (pack_maps + new_maps), 1)},
        "mapTargets": {"canon": canon_targets, "deploymentTypes": len(targets) - canon_targets, "canonPct": round(100 * canon_targets / len(targets), 1)},
        "sourceFields": {"inReboundPackSchemas": pack_fields, "new": new_fields, "reusePct": round(100 * pack_fields / (pack_fields + new_fields), 1)},
        "lines": {"reusedMapLines": pack_map_lines, "authoredOntologyLines": authored_lines, "authoredSpecLines": spec_lines, "packRebindingLines": rebinding_lines,
                  "reusePct": round(100 * pack_map_lines / (pack_map_lines + authored_lines + spec_lines + rebinding_lines), 1)},
        "deployment2Projection": {"newSystemGeneric": len(generic), "newDeploymentSpecific": len(specific),
                                  "reusePctIfGenericPromoted": round(100 * (len(reused) + len(generic)) / total_art, 1),
                                  "genericByReason": {r: sum(1 for a in generic if GENERIC[a["id"]] == r) for r in sorted(set(GENERIC.values()))},
                                  "specific": [a["id"] for a in specific]},
        "newArtifacts": new,
    }
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else None
    text = json.dumps(report, indent=1)
    if out:
        out.write_text(text)
    print(json.dumps({k: v for k, v in report.items() if k not in ("newArtifacts",)}, indent=1))


if __name__ == "__main__":
    main()
