#!/usr/bin/env python3
"""Run bounded OWL 2 RL and SHACL differential checks over the v0 registry."""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import quote

from owlrl import DeductiveClosure, OWLRL_Semantics
from pyshacl import validate
from rdflib import BNode, Graph, Literal, RDF, RDFS, URIRef
from rdflib.namespace import SH, XSD


ROOT = Path(__file__).resolve().parents[3]
REGISTRY = ROOT / "verification/ontology/registry-v0.json"
FIXTURE = ROOT / "verification/ontology/fixtures/registry-conformance-v0.json"
INSTANCE_ROOT = "urn:norn:ontology-differential:"


class SemanticValidationFailure(Exception):
    pass


@dataclass(frozen=True)
class Term:
    term_id: str
    class_iri: URIRef
    shape_iri: URIRef
    shape: dict[str, Any]


def load(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise SemanticValidationFailure(f"{path.relative_to(ROOT)}: root must be an object")
    return value


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SemanticValidationFailure(message)


def local_name(iri: URIRef) -> str:
    text = str(iri)
    return text.rsplit("/", 1)[-1].rsplit("#", 1)[-1]


def load_registry() -> tuple[dict[str, Term], Graph, Graph]:
    registry = load(REGISTRY)
    terms: dict[str, Term] = {}
    ontology = Graph()
    shapes = Graph()
    for bundle in registry["bundles"]:
        directory = ROOT / bundle["directory"]
        pack = load(directory / bundle["artifacts"]["canonical"]["path"])["payload"]
        contract = load(directory / bundle["artifacts"]["projectionContract"]["path"])
        mapping = {item["termId"]: item for item in contract["identityMappings"]}
        shape_by_term = {item["termId"]: item for item in pack["shapes"]}
        ontology.parse(directory / bundle["artifacts"]["owl"]["path"], format="turtle")
        shapes.parse(directory / bundle["artifacts"]["shacl"]["path"], format="turtle")
        for item in pack["terms"]:
            term_id = item["termId"]
            require(term_id not in terms, f"duplicate term {term_id}")
            terms[term_id] = Term(
                term_id=term_id,
                class_iri=URIRef(mapping[term_id]["owlIri"]),
                shape_iri=URIRef(mapping[term_id]["shapeIri"]),
                shape=shape_by_term[term_id],
            )
    return terms, ontology, shapes


def property_iri(shapes: Graph, term: Term, field_id: str) -> URIRef:
    matches: list[URIRef] = []
    for property_node in shapes.objects(term.shape_iri, SH.property):
        path = shapes.value(property_node, SH.path)
        if isinstance(path, URIRef) and local_name(path) == field_id:
            matches.append(path)
    require(len(matches) == 1, f"{term.term_id}.{field_id}: expected one SHACL property path")
    return matches[0]


def literal(value_type: str, value: Any) -> Literal | URIRef:
    if value_type in {"iri", "category_ref", "external_identifier"}:
        return URIRef(str(value))
    if value_type == "boolean":
        return Literal(value, datatype=XSD.boolean)
    if value_type == "integer":
        return Literal(value, datatype=XSD.integer)
    if value_type == "decimal":
        return Literal(str(value), datatype=XSD.decimal)
    if value_type == "date":
        return Literal(str(value), datatype=XSD.date)
    if value_type == "date_time":
        return Literal(str(value), datatype=XSD.dateTime)
    return Literal(str(value), datatype=XSD.string)


def witness_required_fields(
    graph: Graph,
    shapes: Graph,
    terms: dict[str, Term],
    node: URIRef,
    term_id: str,
    stack: set[tuple[URIRef, str]],
) -> None:
    key = (node, term_id)
    if key in stack:
        return
    stack.add(key)
    term = terms[term_id]
    graph.add((node, RDF.type, term.class_iri))
    for field in term.shape["fields"]:
        if int(field["minCount"]) == 0:
            continue
        predicate = property_iri(shapes, term, field["fieldId"])
        if any(graph.objects(node, predicate)):
            continue
        if field["valueType"] == "term_ref":
            target_id = field["targetTermId"]
            target = URIRef(f"{INSTANCE_ROOT}witness:{quote(str(node), safe='')}:{quote(target_id, safe='')}")
            graph.add((node, predicate, target))
            witness_required_fields(graph, shapes, terms, target, target_id, stack)
        else:
            graph.add((node, predicate, literal(field["valueType"], f"witness-{field['fieldId']}")))
    stack.remove(key)


def instance_graph(
    instance: dict[str, Any], terms: dict[str, Term], shapes: Graph
) -> tuple[Graph, dict[str, URIRef]]:
    graph = Graph()
    focus = URIRef(f"{INSTANCE_ROOT}{quote(instance['instanceId'], safe='')}")
    term = terms[instance["termId"]]
    graph.add((focus, RDF.type, term.class_iri))
    predicates: dict[str, URIRef] = {}
    fields = {field["fieldId"]: field for field in term.shape["fields"]}
    for field_id, values in instance["values"].items():
        field = fields[field_id]
        predicate = property_iri(shapes, term, field_id)
        predicates[field_id] = predicate
        for index, value in enumerate(values):
            if field["valueType"] == "term_ref":
                target = URIRef(
                    f"{INSTANCE_ROOT}{quote(instance['instanceId'], safe='')}:{quote(field_id, safe='')}:{index}"
                )
                graph.add((focus, predicate, target))
                witness_required_fields(graph, shapes, terms, target, str(value), set())
            else:
                graph.add((focus, predicate, literal(field["valueType"], value)))
    return graph, predicates


def check_owl_membership(terms: dict[str, Term], ontology: Graph, fixture: dict[str, Any]) -> None:
    DeductiveClosure(
        OWLRL_Semantics,
        axiomatic_triples=False,
        datatype_axioms=True,
    ).expand(ontology)
    for case in fixture["membershipCases"]:
        child = terms[case["termId"]].class_iri
        parent = terms[case["targetTermId"]].class_iri
        entailed = child == parent or (child, RDFS.subClassOf, parent) in ontology
        require(
            entailed is case["expected"],
            f"OWL-RL membership disagreement for {case['name']}: expected {case['expected']}, got {entailed}",
        )


def check_shacl(
    terms: dict[str, Term], ontology: Graph, shapes: Graph, fixture: dict[str, Any]
) -> None:
    for case in fixture["instanceCases"]:
        data, predicates = instance_graph(case["instance"], terms, shapes)
        conforms, report, report_text = validate(
            data_graph=data,
            shacl_graph=shapes,
            ont_graph=ontology,
            # OWL-RL entailment is checked separately above. Data-graph
            # materialization is deliberately disabled: RDFS range inference
            # could otherwise manufacture the very rdf:type required by a
            # sh:class constraint and turn an incompatible closed-world term
            # reference into an apparent match. pySHACL still consults the
            # supplied ontology graph for asserted subclass membership.
            inference="none",
            abort_on_first=False,
            allow_infos=False,
            allow_warnings=False,
            meta_shacl=True,
            advanced=False,
            do_owl_imports=False,
        )
        require(
            bool(conforms) is case["expected"],
            f"SHACL disagreement for {case['name']}: expected {case['expected']}, got {bool(conforms)}\n{report_text}",
        )
        if not case["expected"]:
            field_id = case["expectedPath"].split(".")[-1].split("[")[0]
            expected_path = predicates[field_id]
            result_paths = {path for path in report.objects(None, SH.resultPath) if isinstance(path, URIRef)}
            require(
                expected_path in result_paths,
                f"SHACL rejection for {case['name']} did not identify {field_id}",
            )


def main() -> int:
    fixture = load(FIXTURE)
    terms, ontology, shapes = load_registry()
    check_owl_membership(terms, ontology, fixture)
    check_shacl(terms, ontology, shapes, fixture)
    print(
        f"ontology semantic differential passed: {len(fixture['membershipCases'])} OWL-RL entailments, "
        f"{len(fixture['instanceCases'])} SHACL decisions"
    )
    print("scope: finite reviewed v0 registry fixtures; semantic oracle only, never transition authority")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, KeyError, TypeError, ValueError, SemanticValidationFailure) as error:
        print(f"ontology semantic validation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
