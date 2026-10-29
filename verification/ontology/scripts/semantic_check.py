#!/usr/bin/env python3
"""Run the closed registry, structural, OWL, and SHACL v0 checks."""

from __future__ import annotations

import json
import hashlib
import subprocess
import sys
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[3]
ONTOLOGY_ROOT = ROOT / "verification/ontology"
PACK_DIR = ONTOLOGY_ROOT / "packs/company-commerce-v0"
REGISTRY = ONTOLOGY_ROOT / "registry-v0.json"
PACK = PACK_DIR / "pack.json"
CONTRACT = PACK_DIR / "projection-contract.json"
REPORT = PACK_DIR / "semantic-check-report.json"
TAXONOMY_VERSION = "toy-1"


class SemanticCheckFailure(Exception):
    """A semantic-check step could not complete."""


def load_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise SemanticCheckFailure(f"{path.relative_to(ROOT)}: expected an object")
    return value


def check(name: str, status: str, details: dict[str, Any] | None = None) -> dict[str, Any]:
    value: dict[str, Any] = {"name": name, "status": status}
    if details:
        value["details"] = details
    return value


def run_closed_validator() -> dict[str, Any]:
    result = subprocess.run(
        [sys.executable, str(ONTOLOGY_ROOT / "scripts/validate.py")],
        cwd=ROOT,
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if result.returncode != 0:
        raise SemanticCheckFailure("validate.py returned a non-zero status")
    return check("closed_registry_validator", "passed")


def source_versions(pack: dict[str, Any]) -> dict[str, Any]:
    sources = {source["sourceId"]: source for source in pack["payload"]["sources"]}
    schema = sources["source.schema-org"]
    shopify = sources["source.norn-synthetic-taxonomy"]
    shopify_release = str(shopify["releaseId"]).split("@", 1)[0]
    derived = {
        name: {
            "bytes": path.stat().st_size,
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        }
        for name, path in {
            "categories": PACK_DIR / f"shopify-categories.{TAXONOMY_VERSION}.json",
            "attributes": PACK_DIR / f"shopify-attributes.{TAXONOMY_VERSION}.json",
        }.items()
    }
    schema_file = next(file for file in schema["files"] if file["path"].endswith("schemaorg-current-https.jsonld"))
    raw_categories = next(
        file for file in shopify["files"] if file["path"].endswith("verification/ontology/fixtures/toy-taxonomy/categories.json")
    )
    return {
        "schema.org": {
            "release": schema["releaseId"],
            "retrievedAt": schema["retrievedAt"],
            "sha256": schema_file["sha256"],
        },
        "shopifyProductTaxonomy": {
            "release": shopify_release,
            "retrievedAt": shopify["retrievedAt"],
            "contentSha256": raw_categories["sha256"],
        },
        "derived": derived,
    }


def structural_checks(pack: dict[str, Any], contract: dict[str, Any]) -> dict[str, Any]:
    payload = pack["payload"]
    terms = {term["termId"]: term for term in payload["terms"]}
    shapes = {shape["termId"]: shape for shape in payload["shapes"]}
    identity = {item["termId"]: item for item in contract["identityMappings"]}
    required = {
        "norn.commerce.product": "https://schema.org/Product",
        "norn.commerce.offer": "https://schema.org/Offer",
        "norn.commerce.order": "https://schema.org/Order",
        "norn.commerce.order-item": "https://schema.org/OrderItem",
        "norn.company.organization": "https://schema.org/Organization",
        "norn.company.person": "https://schema.org/Person",
        "norn.company.postal-address": "https://schema.org/PostalAddress",
    }
    missing_classes = [
        term_id
        for term_id, external_iri in required.items()
        if term_id not in terms
        or term_id not in shapes
        or not any(
            mapping["termId"] == term_id and mapping["externalIri"] == external_iri
            for mapping in payload["mappings"]
        )
    ]
    if missing_classes:
        raise SemanticCheckFailure(f"missing required class mappings: {', '.join(missing_classes)}")
    if set(identity) != set(terms) or set(identity) != set(shapes):
        raise SemanticCheckFailure("identity mappings do not cover the active pack terms")
    for term_id, shape in shapes.items():
        if shape["closed"] is not True:
            raise SemanticCheckFailure(f"{term_id}: shape is not closed")
    order_fields = {
        field["fieldId"]: field for field in shapes["norn.commerce.order"]["fields"]
    }
    line_items = order_fields.get("lineItems")
    if line_items != {
        "fieldId": "lineItems",
        "valueType": "term_ref",
        "minCount": 1,
        "maxCount": 100,
        "targetTermId": "norn.commerce.order-item",
    }:
        raise SemanticCheckFailure("Order.lineItems must require one through one hundred OrderItem references")
    category_path = PACK_DIR / f"shopify-categories.{TAXONOMY_VERSION}.json"
    attribute_path = PACK_DIR / f"shopify-attributes.{TAXONOMY_VERSION}.json"
    max_bytes = 5 * 1024 * 1024
    if category_path.stat().st_size > max_bytes or attribute_path.stat().st_size > max_bytes:
        raise SemanticCheckFailure("derived Shopify projection exceeds the 5 MiB limit")
    active_external_iris = [
        mapping["externalIri"]
        for mapping in payload["mappings"]
        if mapping.get("supersededByMappingId") is None
    ]
    if len(active_external_iris) != len(set(active_external_iris)):
        raise SemanticCheckFailure("active mapping external IRIs are not unique")
    forbidden = set(contract["forbiddenUses"])
    required_forbidden = {
        "capability_grant",
        "authorization_decision",
        "transition_guard",
        "authoritative_state_mutation",
    }
    if not required_forbidden <= forbidden:
        raise SemanticCheckFailure("projection contract authority boundary is incomplete")
    return check(
        "closed_pack_structure",
        "passed",
        {
            "terms": len(terms),
            "mappings": len(payload["mappings"]),
            "shapes": len(shapes),
            "requiredClasses": sorted(required),
            "orderItemCardinality": {"minCount": 1, "maxCount": 100},
            "derivedBytes": {
                "categories": category_path.stat().st_size,
                "attributes": attribute_path.stat().st_size,
            },
        },
    )


def local_name(iri: Any) -> str:
    text = str(iri)
    return text.rsplit("/", 1)[-1].rsplit("#", 1)[-1]


def semantic_checks(pack: dict[str, Any]) -> list[dict[str, Any]]:
    try:
        from owlrl import DeductiveClosure, OWLRL_Semantics
        from pyshacl import validate
        from rdflib import BNode, Graph, Literal, RDF, URIRef
        from rdflib.namespace import OWL, SH, XSD
    except ImportError as error:
        return [
            check(
                "owl_shacl_semantics",
                "skipped",
                {"reason": f"optional semantic tools unavailable: {error.name}"},
            )
        ]

    ontology = Graph()
    shapes_graph = Graph()
    ontology.parse(PACK_DIR / "ontology.owl.ttl", format="turtle")
    shapes_graph.parse(PACK_DIR / "shapes.shacl.ttl", format="turtle")
    payload = pack["payload"]
    contract = load_json(CONTRACT)
    identity = {item["termId"]: item for item in contract["identityMappings"]}
    terms = {term["termId"]: term for term in payload["terms"]}
    shapes_by_term = {shape["termId"]: shape for shape in payload["shapes"]}
    term_class = {term_id: URIRef(item["owlIri"]) for term_id, item in identity.items()}
    shape_iri = {term_id: URIRef(item["shapeIri"]) for term_id, item in identity.items()}

    def property_iri(term_id: str, field_id: str) -> URIRef:
        paths = [
            path
            for node in shapes_graph.objects(shape_iri[term_id], SH.property)
            for path in [shapes_graph.value(node, SH.path)]
            if isinstance(path, URIRef) and local_name(path) == field_id
        ]
        if len(paths) != 1:
            raise SemanticCheckFailure(f"{term_id}.{field_id}: expected exactly one SHACL path")
        return paths[0]

    def literal(value_type: str, field_id: str) -> Literal:
        if value_type == "decimal":
            return Literal("1.00", datatype=XSD.decimal)
        if value_type == "date_time":
            return Literal("2026-08-25T00:00:00Z", datatype=XSD.dateTime)
        if value_type == "boolean":
            return Literal(True, datatype=XSD.boolean)
        if value_type == "integer":
            return Literal(1, datatype=XSD.integer)
        return Literal(f"semantic-check-{field_id}", datatype=XSD.string)

    def add_witness(term_id: str, node: URIRef | BNode, graph: Graph, active: set[tuple[str, str]]) -> None:
        key = (term_id, str(node))
        if key in active:
            return
        active.add(key)
        graph.add((node, RDF.type, term_class[term_id]))
        for field in shapes_by_term[term_id]["fields"]:
            if field["minCount"] == 0:
                continue
            predicate = property_iri(term_id, field["fieldId"])
            target_type = field["valueType"]
            for index in range(field["minCount"]):
                if target_type == "term_ref":
                    target_term = field["targetTermId"]
                    if target_term not in term_class:
                        continue
                    target: URIRef | BNode = URIRef(
                        f"urn:norn:semantic-check:{term_id}:{field['fieldId']}:{index}"
                    )
                    graph.add((node, predicate, target))
                    add_witness(target_term, target, graph, active)
                elif target_type in {"external_identifier", "category_ref"}:
                    target_term = (
                        "norn.identity.external-identifier"
                        if target_type == "external_identifier"
                        else "norn.taxonomy.category"
                    )
                    target = BNode(f"{term_id}-{field['fieldId']}-{index}")
                    graph.add((node, predicate, target))
                    add_witness(target_term, target, graph, active)
                elif target_type == "iri":
                    graph.add((node, predicate, URIRef("urn:norn:semantic-check:evidence")))
                else:
                    graph.add((node, predicate, literal(target_type, field["fieldId"])))
        active.remove(key)

    data = Graph()
    roots: dict[str, URIRef] = {}
    for term_id in terms:
        root = URIRef(f"urn:norn:semantic-check:root:{term_id}")
        roots[term_id] = root
        add_witness(term_id, root, data, set())

    conforms, _, report_text = validate(
        data_graph=data,
        shacl_graph=shapes_graph,
        ont_graph=ontology,
        inference="none",
        abort_on_first=False,
        allow_infos=False,
        allow_warnings=False,
        meta_shacl=True,
        advanced=False,
        do_owl_imports=False,
    )
    if not conforms:
        raise SemanticCheckFailure(f"positive SHACL corpus did not conform: {report_text[:500]}")
    negative = Graph()
    for triple in data:
        negative.add(triple)
    line_items = property_iri("norn.commerce.order", "lineItems")
    order_root = roots["norn.commerce.order"]
    for triple in list(negative.triples((order_root, line_items, None))):
        negative.remove(triple)
    negative_conforms, _, _ = validate(
        data_graph=negative,
        shacl_graph=shapes_graph,
        ont_graph=ontology,
        inference="none",
        abort_on_first=False,
        allow_infos=False,
        allow_warnings=False,
        meta_shacl=True,
        advanced=False,
        do_owl_imports=False,
    )
    if negative_conforms:
        raise SemanticCheckFailure("negative Order corpus was accepted without lineItems")

    combined = Graph()
    for triple in ontology:
        combined.add(triple)
    for triple in data:
        combined.add(triple)
    DeductiveClosure(
        OWLRL_Semantics,
        axiomatic_triples=False,
        datatype_axioms=True,
    ).expand(combined)
    nothing_members = list(combined.triples((None, RDF.type, OWL.Nothing)))
    disjoint_pairs = {
        (str(subject), str(target))
        for subject, _, target in ontology.triples((None, OWL.disjointWith, None))
    }
    if any(subject == target for subject, target in disjoint_pairs) or nothing_members:
        raise SemanticCheckFailure("OWL closure contains a disjointness inconsistency")
    return [
        check(
            "shacl_conformance",
            "passed",
            {"positiveInstances": len(terms), "negativeOrderLineItems": "rejected"},
        ),
        check(
            "owl_consistency_disjointness",
            "passed",
            {"disjointPairs": len(disjoint_pairs), "owlNothingInstances": 0},
        ),
    ]


def main() -> int:
    checks: list[dict[str, Any]] = []
    versions: dict[str, Any] = {}
    try:
        pack = load_json(PACK)
        contract = load_json(CONTRACT)
        versions = source_versions(pack)
        try:
            checks.append(run_closed_validator())
        except SemanticCheckFailure as error:
            checks.append(check("closed_registry_validator", "failed", {"reason": str(error)}))
        try:
            checks.append(structural_checks(pack, contract))
        except (KeyError, OSError, SemanticCheckFailure) as error:
            checks.append(check("closed_pack_structure", "failed", {"reason": str(error)}))
        if not any(item["name"] == "closed_registry_validator" and item["status"] == "failed" for item in checks):
            try:
                checks.extend(semantic_checks(pack))
            except (KeyError, OSError, SemanticCheckFailure, ValueError) as error:
                checks.append(check("owl_shacl_semantics", "failed", {"reason": str(error)}))
    except (KeyError, OSError, TypeError, ValueError, SemanticCheckFailure) as error:
        checks.append(check("semantic_check_input", "failed", {"reason": str(error)}))

    status = "failed" if any(item["status"] == "failed" for item in checks) else "semantic-checked"
    REPORT.write_text(
        json.dumps({"status": status, "checks": checks, "sourceVersions": versions}, indent=2) + "\n",
        encoding="utf-8",
    )
    return 0 if status == "semantic-checked" else 1


if __name__ == "__main__":
    raise SystemExit(main())
