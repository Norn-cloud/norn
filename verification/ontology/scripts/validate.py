#!/usr/bin/env python3
"""Validate the bounded, checked-in Norn ontology registry profile.

This is deliberately not an RDF parser, OWL reasoner, or SHACL engine. It
checks artifact pins and the exact static projection statements admitted by
`reviewed_static_v0`; semantic reasoning remains an explicit open obligation.
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
from datetime import datetime
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[3]
REGISTRY_PATH = ROOT / "verification/ontology/registry-v0.json"
IMPLEMENTED_STATUSES = {
    "implemented_executable",
    "implemented_bounded_static_checker",
    "implemented_bounded_external_reasoner",
    "implemented_bounded_shacl_differential",
}
PARTIAL_STATUSES = {"partial_executable"}
OPEN_STATUSES = {
    "open_foundational_proof",
    "open_no_exact_mapping_claims_v0",
}
SEMANTIC_CHECKERS = {
    "scripts/check-ontology-semantics.sh",
    "verification/ontology/scripts/semantic_check.py",
}


class ValidationFailure(Exception):
    pass


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValidationFailure(f"{path.relative_to(ROOT)}: cannot load JSON: {error}") from error
    if not isinstance(value, dict):
        raise ValidationFailure(f"{path.relative_to(ROOT)}: root must be an object")
    return value


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValidationFailure(message)


def prefixes(text: str, relative_path: str) -> dict[str, str]:
    found = dict(re.findall(r"@prefix\s+([A-Za-z][\w-]*):\s*<([^>]+)>\s*\.", text))
    require("norn" in found, f"{relative_path}: missing norn prefix")
    return found


def iri_token(iri: str, declared: dict[str, str], relative_path: str) -> str:
    candidates = [
        (len(namespace), f"{prefix}:{iri[len(namespace):]}")
        for prefix, namespace in declared.items()
        if iri.startswith(namespace) and iri != namespace
    ]
    require(bool(candidates), f"{relative_path}: no declared prefix covers projection IRI {iri}")
    return max(candidates)[1]


def normalized(text: str) -> str:
    without_comment_lines = "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("#")
    )
    return re.sub(r"\s+", " ", without_comment_lines).strip()


def statement_block(text: str, subject: str, relative_path: str) -> str:
    for block in re.split(r"\n\s*\n", text):
        if re.search(rf"(?:^|\n)\s*{re.escape(subject)}\s+a\s+", block):
            return normalized(block)
    raise ValidationFailure(f"{relative_path}: no statement block for {subject}")


def validate_obligations(path: Path, pack_id: str, pack_version: str) -> None:
    document = load_json(path)
    relative = str(path.relative_to(ROOT))
    require(document.get("packId") == pack_id, f"{relative}: packId mismatch")
    require(document.get("packVersion") == pack_version, f"{relative}: packVersion mismatch")
    vocabulary = document.get("statusVocabulary")
    require(isinstance(vocabulary, list) and vocabulary, f"{relative}: statusVocabulary must be non-empty")
    obligations = document.get("obligations")
    require(isinstance(obligations, list) and obligations, f"{relative}: obligations must be non-empty")
    seen: set[str] = set()
    for index, obligation in enumerate(obligations):
        prefix = f"{relative}: obligations[{index}]"
        require(isinstance(obligation, dict), f"{prefix} must be an object")
        obligation_id = obligation.get("obligationId")
        status = obligation.get("status")
        require(isinstance(obligation_id, str) and obligation_id, f"{prefix}: missing obligationId")
        require(obligation_id not in seen, f"{prefix}: duplicate obligationId {obligation_id}")
        seen.add(obligation_id)
        require(status in vocabulary, f"{prefix}: status is outside statusVocabulary")
        require(
            status in IMPLEMENTED_STATUSES | PARTIAL_STATUSES | OPEN_STATUSES,
            f"{prefix}: unsupported status {status}",
        )
        implemented_by = obligation.get("implementedBy")
        evidence = obligation.get("evidence")
        require(isinstance(implemented_by, list), f"{prefix}: implementedBy must be an array")
        require(isinstance(evidence, list), f"{prefix}: evidence must be an array")
        if status in IMPLEMENTED_STATUSES | PARTIAL_STATUSES:
            require(bool(implemented_by), f"{prefix}: implemented status requires implementedBy")
            require(bool(evidence), f"{prefix}: implemented status requires evidence")
            for evidence_index, evidence_path in enumerate(evidence):
                evidence_prefix = f"{prefix}: evidence[{evidence_index}]"
                require(
                    isinstance(evidence_path, str)
                    and evidence_path
                    and not evidence_path.startswith("/")
                    and ".." not in Path(evidence_path).parts,
                    f"{evidence_prefix}: must be a safe repo-relative path",
                )
                require(
                    (ROOT / evidence_path).is_file(),
                    f"{evidence_prefix}: artifact does not exist: {evidence_path}",
                )
        if status in PARTIAL_STATUSES:
            residual = obligation.get("residual")
            require(
                isinstance(residual, str) and residual.strip(),
                f"{prefix}: partial status requires a non-empty residual",
            )


def valid_utc_date_time(value: str) -> bool:
    match = re.fullmatch(
        r"(\d{4}-\d{2}-\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?Z",
        value,
    )
    if match is None:
        return False
    try:
        datetime.strptime(match.group(1), "%Y-%m-%d")
    except ValueError:
        return False
    return (
        int(match.group(2)) <= 23
        and int(match.group(3)) <= 59
        and int(match.group(4)) <= 59
    )


def validate_mapping_provenance(pack: dict[str, Any], relative_pack: str) -> None:
    mappings = pack.get("mappings")
    require(isinstance(mappings, list) and mappings, f"{relative_pack}: mappings must be non-empty")
    records: dict[str, dict[str, Any]] = {}
    active_external_iris: set[str] = set()
    for index, mapping in enumerate(mappings):
        prefix = f"{relative_pack}: mappings[{index}]"
        require(isinstance(mapping, dict), f"{prefix} must be an object")
        mapping_id = mapping.get("mappingId")
        require(isinstance(mapping_id, str) and mapping_id.strip(), f"{prefix}: mappingId is required")
        require(mapping_id not in records, f"{prefix}: duplicate mappingId {mapping_id}")
        external_iri = mapping.get("externalIri")
        require(
            isinstance(external_iri, str) and external_iri.strip(),
            f"{prefix}: externalIri is required",
        )
        if mapping.get("supersededByMappingId") is None:
            require(
                external_iri not in active_external_iris,
                f"{prefix}: externalIri must be unique among active mappings",
            )
            active_external_iris.add(external_iri)
        confidence = mapping.get("confidenceBasisPoints")
        require(
            type(confidence) is int and 0 <= confidence <= 10_000,
            f"{prefix}: confidenceBasisPoints must be an integer from 0 through 10000",
        )
        reviewed_by = mapping.get("reviewedBy")
        require(
            isinstance(reviewed_by, str) and reviewed_by.strip(),
            f"{prefix}: reviewedBy must be a non-empty string",
        )
        reviewed_at = mapping.get("reviewedAt")
        require(
            isinstance(reviewed_at, str) and valid_utc_date_time(reviewed_at),
            f"{prefix}: reviewedAt must use the supported UTC RFC3339 subset",
        )
        successor = mapping.get("supersededByMappingId")
        require(
            successor is None or isinstance(successor, str) and successor.strip(),
            f"{prefix}: supersededByMappingId must be a non-empty string when present",
        )
        records[mapping_id] = mapping

    for mapping_id, mapping in records.items():
        successor_id = mapping.get("supersededByMappingId")
        if successor_id is None:
            continue
        require(successor_id != mapping_id, f"{relative_pack}: mapping {mapping_id} cannot supersede itself")
        successor = records.get(successor_id)
        require(successor is not None, f"{relative_pack}: unresolved mapping successor {successor_id}")
        require(
            successor.get("supersededByMappingId") is None,
            f"{relative_pack}: mapping {mapping_id} must resolve directly to an active replacement",
        )
        require(
            successor.get("termId") == mapping.get("termId")
            and successor.get("sourceId") == mapping.get("sourceId"),
            f"{relative_pack}: mapping {mapping_id} replacement must preserve termId and sourceId",
        )


def validate_bundle(bundle: dict[str, Any], global_terms: dict[str, dict[str, Any]]) -> None:
    directory = ROOT / str(bundle["directory"])
    artifacts = bundle.get("artifacts")
    require(isinstance(artifacts, dict), f"{bundle['directory']}: artifacts must be an object")
    paths: dict[str, Path] = {}
    for role in ("canonical", "owl", "shacl", "projectionContract", "proofObligations"):
        artifact = artifacts.get(role)
        require(isinstance(artifact, dict), f"{bundle['directory']}: missing artifact role {role}")
        path = directory / str(artifact.get("path", ""))
        require(path.is_file(), f"{path.relative_to(ROOT)}: artifact does not exist")
        expected = artifact.get("sha256")
        require(isinstance(expected, str) and re.fullmatch(r"[0-9a-f]{64}", expected) is not None,
                f"{path.relative_to(ROOT)}: registry SHA-256 is invalid")
        require(sha256(path) == expected, f"{path.relative_to(ROOT)}: registry SHA-256 mismatch")
        paths[role] = path

    envelope = load_json(paths["canonical"])
    pack = envelope.get("payload")
    relative_pack = str(paths["canonical"].relative_to(ROOT))
    require(isinstance(pack, dict), f"{relative_pack}: payload must be an object")
    pack_id = str(bundle["packId"])
    pack_version = str(bundle["packVersion"])
    require(pack.get("packId") == pack_id, f"{relative_pack}: packId mismatch")
    require(pack.get("packVersion") == pack_version, f"{relative_pack}: packVersion mismatch")
    if "lifecycle" in bundle:
        require(
            bundle.get("lifecycle") == envelope.get("lifecycle"),
            f"{relative_pack}: registry lifecycle must match the artifact lifecycle",
        )
    if "admission" in bundle:
        require(
            bundle.get("admission") == "admitted_for_semantic_validation",
            f"{relative_pack}: unsupported registry admission state",
        )
    for source_index, source in enumerate(pack.get("sources", [])):
        require(isinstance(source, dict), f"{relative_pack}: sources[{source_index}] must be an object")
        files = source.get("files", [])
        require(isinstance(files, list), f"{relative_pack}: sources[{source_index}].files must be an array")
        for file_index, file in enumerate(files):
            prefix = f"{relative_pack}: sources[{source_index}].files[{file_index}]"
            require(isinstance(file, dict), f"{prefix} must be an object")
            file_path = file.get("path")
            file_hash = file.get("sha256")
            require(isinstance(file_path, str) and file_path.strip(), f"{prefix}.path is required")
            require(
                isinstance(file_hash, str) and re.fullmatch(r"[0-9a-f]{64}", file_hash) is not None,
                f"{prefix}.sha256 must be lowercase SHA-256",
            )
            if "://" in file_path:
                continue
            local_path = ROOT / file_path
            require(
                not Path(file_path).is_absolute() and ".." not in Path(file_path).parts,
                f"{prefix}.path must be a safe repo-relative path or an absolute URL",
            )
            require(local_path.is_file(), f"{prefix}.path does not exist: {file_path}")
            require(sha256(local_path) == file_hash, f"{prefix}.sha256 does not match {file_path}")
    validate_mapping_provenance(pack, relative_pack)

    contract = load_json(paths["projectionContract"])
    relative_contract = str(paths["projectionContract"].relative_to(ROOT))
    require(contract.get("packId") == pack_id, f"{relative_contract}: packId mismatch")
    require(contract.get("packVersion") == pack_version, f"{relative_contract}: packVersion mismatch")
    require(contract.get("projectionMethod") == "reviewed_static_v0",
            f"{relative_contract}: unsupported projectionMethod")
    require(contract.get("reproductionChecker") == "verification/ontology/scripts/validate.py",
            f"{relative_contract}: wrong reproductionChecker")
    semantic_differential = contract.get("semanticDifferential")
    require(isinstance(semantic_differential, dict),
            f"{relative_contract}: semanticDifferential must be an object")
    require(semantic_differential.get("checker") in SEMANTIC_CHECKERS,
            f"{relative_contract}: unsupported semantic differential checker")
    require(semantic_differential.get("fixture") ==
            "verification/ontology/fixtures/registry-conformance-v0.json",
            f"{relative_contract}: wrong semantic differential fixture")
    require(semantic_differential.get("toolLock") == "verification/ontology/tools/requirements.lock",
            f"{relative_contract}: wrong semantic differential tool lock")
    require(semantic_differential.get("owlProfile") == "OWL 2 RL via owlrl 7.6.2",
            f"{relative_contract}: unpinned OWL profile")
    require(semantic_differential.get("shaclProfile") ==
            "SHACL 1.0 via pyshacl 0.40.1 with data inference disabled",
            f"{relative_contract}: unpinned SHACL profile")
    require(contract.get("canonicalArtifact") == paths["canonical"].name,
            f"{relative_contract}: canonical artifact path mismatch")
    require(contract.get("canonicalArtifactSha256") == sha256(paths["canonical"]),
            f"{relative_contract}: canonical artifact hash mismatch")
    for role, key in (("owl", "owlProjection"), ("shacl", "shaclProjection")):
        projection = contract.get(key)
        require(isinstance(projection, dict), f"{relative_contract}: missing {key}")
        require(projection.get("path") == paths[role].name, f"{relative_contract}: {key} path mismatch")
        require(projection.get("sha256") == sha256(paths[role]), f"{relative_contract}: {key} hash mismatch")
    require(contract.get("owlProjection", {}).get("remoteImportsAllowed") is False,
            f"{relative_contract}: remote OWL imports must be false")
    require(contract.get("shaclProjection", {}).get("promotionAuthority") is False,
            f"{relative_contract}: SHACL promotion authority must be false")
    forbidden_uses = contract.get("forbiddenUses")
    require(isinstance(forbidden_uses, list), f"{relative_contract}: forbiddenUses must be an array")
    require(
        {"capability_grant", "authorization_decision", "transition_guard", "authoritative_state_mutation"}
        <= set(forbidden_uses),
        f"{relative_contract}: authority-boundary forbidden uses are incomplete",
    )

    mappings = contract.get("identityMappings")
    require(isinstance(mappings, list), f"{relative_contract}: identityMappings must be an array")
    mapping_by_term: dict[str, dict[str, str]] = {}
    owl_iris: set[str] = set()
    shape_iris: set[str] = set()
    for mapping in mappings:
        require(isinstance(mapping, dict), f"{relative_contract}: mapping must be an object")
        term_id = mapping.get("termId")
        owl_iri = mapping.get("owlIri")
        shape_iri = mapping.get("shapeIri")
        require(isinstance(term_id, str) and term_id not in mapping_by_term,
                f"{relative_contract}: duplicate or missing term mapping")
        require(isinstance(owl_iri, str) and owl_iri not in owl_iris,
                f"{relative_contract}: duplicate or missing OWL IRI")
        require(isinstance(shape_iri, str) and shape_iri not in shape_iris,
                f"{relative_contract}: duplicate or missing shape IRI")
        mapping_by_term[term_id] = {"owlIri": owl_iri, "shapeIri": shape_iri}
        owl_iris.add(owl_iri)
        shape_iris.add(shape_iri)

    terms = pack.get("terms")
    shapes = pack.get("shapes")
    require(isinstance(terms, list) and isinstance(shapes, list), f"{relative_pack}: terms/shapes must be arrays")
    term_ids = {term.get("termId") for term in terms if isinstance(term, dict)}
    shaped_ids = {shape.get("termId") for shape in shapes if isinstance(shape, dict)}
    require(set(mapping_by_term) == term_ids == shaped_ids,
            f"{relative_contract}: identity mappings must exactly cover pack terms and shapes")

    owl_text = paths["owl"].read_text(encoding="utf-8")
    shacl_text = paths["shacl"].read_text(encoding="utf-8")
    relative_owl = str(paths["owl"].relative_to(ROOT))
    relative_shacl = str(paths["shacl"].relative_to(ROOT))
    require("owl:imports" not in owl_text, f"{relative_owl}: owl:imports is forbidden")
    owl_prefixes = prefixes(owl_text, relative_owl)
    shacl_prefixes = prefixes(shacl_text, relative_shacl)

    for term in terms:
        require(isinstance(term, dict), f"{relative_pack}: term must be an object")
        term_id = str(term["termId"])
        mapping = mapping_by_term[term_id]
        owl_token = iri_token(mapping["owlIri"], owl_prefixes, relative_owl)
        owl_block = statement_block(owl_text, owl_token, relative_owl)
        require(f"{owl_token} a owl:Class" in owl_block,
                f"{relative_owl}: {term_id} is not projected as owl:Class")
        parent_id = term.get("parentTermId")
        if isinstance(parent_id, str):
            parent_mapping = global_terms.get(parent_id)
            require(parent_mapping is not None, f"{relative_pack}: unresolved parentTermId {parent_id}")
            parent_token = iri_token(str(parent_mapping["owlIri"]), owl_prefixes, relative_owl)
            require(f"rdfs:subClassOf {parent_token}" in owl_block,
                    f"{relative_owl}: {term_id} parent projection mismatch")

        shape_token = iri_token(mapping["shapeIri"], shacl_prefixes, relative_shacl)
        shape_block = statement_block(shacl_text, shape_token, relative_shacl)
        class_token = iri_token(mapping["owlIri"], shacl_prefixes, relative_shacl)
        require(f"{shape_token} a sh:NodeShape" in shape_block,
                f"{relative_shacl}: {term_id} is not projected as a node shape")
        require(f"sh:targetClass {class_token}" in shape_block,
                f"{relative_shacl}: {term_id} targetClass mismatch")
        require("sh:closed true" in shape_block, f"{relative_shacl}: {term_id} shape is not closed")

        shape = next(item for item in shapes if isinstance(item, dict) and item.get("termId") == term_id)
        for field in shape.get("fields", []):
            if not isinstance(field, dict) or field.get("valueType") != "term_ref":
                continue
            target_id = field.get("targetTermId")
            target = global_terms.get(str(target_id))
            require(target is not None and target.get("status") == "active",
                    f"{relative_pack}: unresolved active targetTermId {target_id}")
            property_token = f"norn:{field['fieldId']}"
            target_owl_token = iri_token(str(target["owlIri"]), owl_prefixes, relative_owl)
            require(
                re.search(
                    rf"{re.escape(property_token)}\s+a\s+owl:ObjectProperty\s*;\s*rdfs:range\s+{re.escape(target_owl_token)}",
                    normalized(owl_text),
                ) is not None,
                f"{relative_owl}: {term_id}.{field['fieldId']} range mismatch",
            )
            target_shacl_token = iri_token(str(target["owlIri"]), shacl_prefixes, relative_shacl)
            require(
                re.search(
                    rf"sh:path\s+{re.escape(property_token)}\s*;\s*sh:class\s+{re.escape(target_shacl_token)}",
                    shape_block,
                ) is not None,
                f"{relative_shacl}: {term_id}.{field['fieldId']} class mismatch",
            )

    validate_obligations(paths["proofObligations"], pack_id, pack_version)


def main() -> int:
    try:
        registry = load_json(REGISTRY_PATH)
        require(registry.get("registryVersion") == "0.1", "registry: unsupported registryVersion")
        require(registry.get("checkerProfile") == "bounded_static_plus_semantic_differential_v0",
                "registry: unsupported checkerProfile")
        require(registry.get("authority") == "semantic_validation_only",
                "registry: authority must remain semantic_validation_only")
        bundles = registry.get("bundles")
        require(isinstance(bundles, list) and bundles, "registry: bundles must be non-empty")

        global_terms: dict[str, dict[str, Any]] = {}
        for bundle in bundles:
            require(isinstance(bundle, dict), "registry: bundle must be an object")
            canonical = bundle.get("artifacts", {}).get("canonical", {})
            path = ROOT / str(bundle.get("directory", "")) / str(canonical.get("path", ""))
            envelope = load_json(path)
            mappings_path = ROOT / str(bundle["directory"]) / str(
                bundle.get("artifacts", {}).get("projectionContract", {}).get("path", "")
            )
            contract = load_json(mappings_path)
            mappings = {
                mapping["termId"]: mapping
                for mapping in contract.get("identityMappings", [])
                if isinstance(mapping, dict) and isinstance(mapping.get("termId"), str)
            }
            for term in envelope.get("payload", {}).get("terms", []):
                require(isinstance(term, dict) and isinstance(term.get("termId"), str),
                        f"{path.relative_to(ROOT)}: malformed term")
                term_id = str(term["termId"])
                require(term_id not in global_terms, f"registry: duplicate global termId {term_id}")
                require(term_id in mappings, f"{mappings_path.relative_to(ROOT)}: missing mapping for {term_id}")
                global_terms[term_id] = {**term, **mappings[term_id]}

        for bundle in bundles:
            validate_bundle(bundle, global_terms)

        for fixture in registry.get("sharedFixtures", []):
            require(isinstance(fixture, dict), "registry: shared fixture entry must be an object")
            path = ROOT / str(fixture.get("path", ""))
            require(path.is_file(), f"{path.relative_to(ROOT)}: shared fixture does not exist")
            require(sha256(path) == fixture.get("sha256"), f"{path.relative_to(ROOT)}: SHA-256 mismatch")
    except (KeyError, TypeError, ValidationFailure, OSError) as error:
        print(f"ontology registry validation failed: {error}", file=sys.stderr)
        return 1

    print(f"ontology registry validation passed: {len(global_terms)} terms, {len(bundles)} packs")
    print("scope: static hashes + closed registry + reviewed projection statements; semantic tools run in the separate differential gate")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
