#!/usr/bin/env python3
"""Build the checked-in company-commerce v0 pack from pinned projections."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[3]
PACK_DIR = ROOT / "verification/ontology/packs/company-commerce-v0"
TAXONOMY_VERSION = "toy-1"
SCHEMA_VERSION = "30.0"
RETRIEVED_DATE = "2026-08-25"
REVIEWED_AT = "2026-08-25T00:00:00Z"
SCHEMA_JSONLD_SHA256 = "4467fa19edcb1d7fb3c46c0adf3591b7f870c4a60b7838bdb61694fd02864cf6"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_json(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError(f"{path}: expected an object")
    return value


def taxonomy_numeric_id(iri: str) -> str:
    return iri.rsplit("/", 1)[-1]


def field(
    field_id: str,
    value_type: str,
    min_count: int,
    max_count: int,
    target_term_id: str | None = None,
) -> dict[str, Any]:
    value: dict[str, Any] = {
        "fieldId": field_id,
        "valueType": value_type,
        "minCount": min_count,
        "maxCount": max_count,
    }
    if target_term_id is not None:
        value["targetTermId"] = target_term_id
    return value


def shape(shape_id: str, term_id: str, fields: list[dict[str, Any]]) -> dict[str, Any]:
    return {"shapeId": shape_id, "termId": term_id, "closed": True, "fields": fields}


def mapping(
    mapping_id: str,
    term_id: str,
    source_id: str,
    external_iri: str,
    relation: str,
    justification: str,
    confidence: int,
    superseded_by: str | None = None,
) -> dict[str, Any]:
    value: dict[str, Any] = {
        "mappingId": mapping_id,
        "termId": term_id,
        "sourceId": source_id,
        "externalIri": external_iri,
        "relation": relation,
        "justification": justification,
        "evidenceRefs": [external_iri],
        "confidenceBasisPoints": confidence,
        "reviewedBy": "unreviewed",
        "reviewedAt": REVIEWED_AT,
    }
    if superseded_by is not None:
        value["supersededByMappingId"] = superseded_by
    return value


def build_sources() -> list[dict[str, Any]]:
    categories_path = ROOT / "verification/ontology/fixtures/toy-taxonomy/categories.json"
    attributes_path = ROOT / "verification/ontology/fixtures/toy-taxonomy/attributes.json"
    values_path = ROOT / "verification/ontology/fixtures/toy-taxonomy/attribute_values.json"
    category_projection = PACK_DIR / f"shopify-categories.{TAXONOMY_VERSION}.json"
    attribute_projection = PACK_DIR / f"shopify-attributes.{TAXONOMY_VERSION}.json"
    return [
        {
            "sourceId": "source.schema-org",
            "releaseId": SCHEMA_VERSION,
            "releasedAt": "2026-03-19",
            "retrievedAt": "2026-10-31",
            "canonicalUrl": "https://schema.org/version/30.0/",
            "license": "CC-BY-SA-3.0",
            "redistribution": "reference_only",
            "importMode": "map_only",
            "files": [
                {
                    "path": "https://raw.githubusercontent.com/schemaorg/schemaorg/420231f6bfac8372fc564abb121fae57ccb36a0c/data/releases/30.0/schemaorg-current-https.jsonld",
                    "sha256": SCHEMA_JSONLD_SHA256,
                }
            ],
        },
        {
            "sourceId": "source.norn-synthetic-taxonomy",
            "releaseId": f"{TAXONOMY_VERSION}@{sha256(categories_path)}",
            "releasedAt": RETRIEVED_DATE,
            "retrievedAt": RETRIEVED_DATE,
            "canonicalUrl": "https://example.invalid/norn/toy-taxonomy/1/",
            "license": "Apache-2.0",
            "redistribution": "vendored",
            "importMode": "import",
            "importer": {"name": "norn.ontology.company-commerce-builder", "version": "0.1.0"},
            "files": [
                {"path": str(categories_path.relative_to(ROOT)), "sha256": sha256(categories_path)},
                {"path": str(attributes_path.relative_to(ROOT)), "sha256": sha256(attributes_path)},
                {"path": str(values_path.relative_to(ROOT)), "sha256": sha256(values_path)},
                {"path": str(category_projection.relative_to(ROOT)), "sha256": sha256(category_projection)},
                {"path": str(attribute_projection.relative_to(ROOT)), "sha256": sha256(attribute_projection)},
            ],
        },
    ]


def build_terms() -> list[dict[str, Any]]:
    terms = [
        (
            "norn.company.party",
            "Party",
            "A person or organization represented in the operating model.",
            "class",
            None,
        ),
        (
            "norn.company.legal-entity",
            "Legal Entity",
            "A party recognized as a legal entity in a declared jurisdiction.",
            "class",
            "norn.company.party",
        ),
        (
            "norn.company.organization",
            "Organization",
            "An organization participating in a commerce operating model.",
            "class",
            "norn.company.party",
        ),
        (
            "norn.company.person",
            "Person",
            "A person participating in a commerce operating model.",
            "class",
            "norn.company.party",
        ),
        (
            "norn.company.postal-address",
            "Postal Address",
            "A mailing address associated with a party or order.",
            "value_object",
            None,
        ),
        (
            "norn.commerce.product",
            "Product",
            "A commercial product concept independent of a sellable offer.",
            "class",
            None,
        ),
        (
            "norn.commerce.product-variant",
            "Product Variant",
            "A particular variation of a product that can carry identifiers and categories.",
            "class",
            "norn.commerce.product",
        ),
        (
            "norn.commerce.offer",
            "Offer",
            "A sellable offer for a product with price and availability context.",
            "class",
            None,
        ),
        (
            "norn.commerce.order",
            "Order",
            "A commerce order containing one or more order items.",
            "class",
            None,
        ),
        (
            "norn.commerce.order-item",
            "Order Item",
            "A line item in an order that identifies a product and quantity.",
            "class",
            None,
        ),
        (
            "norn.logistics.trade-item",
            "Trade Item",
            "A product or service unit identified for trade and logistics handling.",
            "class",
            None,
        ),
        (
            "norn.taxonomy.category",
            "Category",
            "A Norn-owned classification concept that may map to external taxonomies.",
            "concept_scheme_root",
            None,
        ),
        (
            "norn.identity.external-identifier",
            "External Identifier",
            "A scoped identifier or additional-property value issued under an external scheme.",
            "value_object",
            None,
        ),
        (
            "norn.evidence.provenance-record",
            "Provenance Record",
            "Evidence describing the source and observation context of a semantic claim.",
            "value_object",
            None,
        ),
    ]
    result = []
    for term_id, label, definition, term_kind, parent in terms:
        item: dict[str, Any] = {
            "termId": term_id,
            "label": label,
            "definition": definition,
            "termKind": term_kind,
            "status": "active",
            "semanticAuthority": "semantic_only",
        }
        if parent is not None:
            item["parentTermId"] = parent
        result.append(item)
    return result


def build_mappings(categories: dict[str, Any], attributes: dict[str, Any]) -> list[dict[str, Any]]:
    schema = "https://schema.org/"
    shopify = "https://example.invalid/norn/toy-taxonomy/1/"
    result = [
        mapping(
            "mapping.organization.schema",
            "norn.company.organization",
            "source.schema-org",
            f"{schema}Organization",
            "close_match",
            "Schema.org Organization supplies the v0 interoperability class while Norn retains its own party hierarchy.",
            9000,
        ),
        mapping(
            "mapping.person.schema",
            "norn.company.person",
            "source.schema-org",
            f"{schema}Person",
            "close_match",
            "Schema.org Person supplies the v0 interoperability class while Norn retains its own party hierarchy.",
            9000,
        ),
        mapping(
            "mapping.postal-address.schema",
            "norn.company.postal-address",
            "source.schema-org",
            f"{schema}PostalAddress",
            "close_match",
            "Schema.org PostalAddress supplies mailing-address interoperability without granting operational authority.",
            9000,
        ),
        mapping(
            "mapping.product.schema",
            "norn.commerce.product",
            "source.schema-org",
            f"{schema}Product",
            "close_match",
            "Schema.org Product overlaps the Norn product concept; Norn owns the closed import shape and lifecycle.",
            9000,
        ),
        mapping(
            "mapping.offer.schema",
            "norn.commerce.offer",
            "source.schema-org",
            f"{schema}Offer",
            "close_match",
            "Schema.org Offer supplies price and availability interoperability for a Norn offer.",
            9000,
        ),
        mapping(
            "mapping.order.schema",
            "norn.commerce.order",
            "source.schema-org",
            f"{schema}Order",
            "close_match",
            "Schema.org Order supplies order-document interoperability while Norn owns order transitions.",
            9000,
        ),
        mapping(
            "mapping.order-item.schema",
            "norn.commerce.order-item",
            "source.schema-org",
            f"{schema}OrderItem",
            "close_match",
            "Schema.org OrderItem supplies line-item interoperability while Norn owns order membership rules.",
            9000,
        ),
        mapping(
            "mapping.external-identifier.schema-property-value",
            "norn.identity.external-identifier",
            "source.schema-org",
            f"{schema}PropertyValue",
            "related_match",
            "Schema.org PropertyValue carries named values; Norn constrains the same carrier for identifiers and taxonomy properties.",
            8000,
        ),
        mapping(
            "mapping.category.shopify-root",
            "norn.taxonomy.category",
            "source.norn-synthetic-taxonomy",
            f"{shopify}?categoryId=sg",
            "related_match",
            "The prior Shopify root mapping is retained as a direct superseded record for the same Norn category scheme.",
            7000,
            "mapping.category.shopify-apparel-root",
        ),
        mapping(
            "mapping.category.shopify-apparel-root",
            "norn.taxonomy.category",
            "source.norn-synthetic-taxonomy",
            f"{shopify}?categoryId=aa",
            "related_match",
            "Apparel & Accessories is the v0 category root used by the commerce registry projection.",
            7000,
        ),
    ]
    roots = [category for category in categories["categories"] if category["level"] == 0]
    for category in roots:
        root_id = taxonomy_numeric_id(category["id"])
        if root_id in {"aa", "sg"}:
            continue
        result.append(
            mapping(
                f"mapping.category.shopify.{root_id}-root",
                "norn.taxonomy.category",
                "source.norn-synthetic-taxonomy",
                f"{shopify}?categoryId={root_id}",
                "related_match",
                f"Shopify {category['fullPath']} is registered as an external category root; Norn retains category identity.",
                7000,
            )
        )
    for attribute in attributes["attributes"]:
        attribute_id = taxonomy_numeric_id(attribute["id"])
        external_iri = f"{shopify}?attributeId={attribute_id}"
        result.append(
            mapping(
                f"mapping.attribute.shopify.{attribute['handle']}",
                "norn.identity.external-identifier",
                "source.norn-synthetic-taxonomy",
                external_iri,
                "related_match",
                f"Shopify attribute {attribute['name']} maps to the Norn additionalProperty carrier; values remain source-scoped.",
                7000,
            )
        )
    return result


def build_shapes() -> list[dict[str, Any]]:
    return [
        shape(
            "shape.company.party",
            "norn.company.party",
            [
                field("displayName", "string", 1, 1),
                field("externalIdentifiers", "external_identifier", 0, 100),
            ],
        ),
        shape(
            "shape.company.legal-entity",
            "norn.company.legal-entity",
            [
                field("displayName", "string", 1, 1),
                field("legalName", "string", 1, 1),
                field("jurisdictionCode", "string", 1, 1),
                field("externalIdentifiers", "external_identifier", 0, 100),
            ],
        ),
        shape(
            "shape.company.organization",
            "norn.company.organization",
            [
                field("displayName", "string", 1, 1),
                field("legalName", "string", 0, 1),
                field("address", "term_ref", 0, 1, "norn.company.postal-address"),
                field("email", "string", 0, 1),
                field("telephone", "string", 0, 1),
                field("externalIdentifiers", "external_identifier", 0, 100),
            ],
        ),
        shape(
            "shape.company.person",
            "norn.company.person",
            [
                field("displayName", "string", 1, 1),
                field("givenName", "string", 0, 1),
                field("familyName", "string", 0, 1),
                field("email", "string", 0, 1),
                field("telephone", "string", 0, 1),
                field("address", "term_ref", 0, 1, "norn.company.postal-address"),
                field("externalIdentifiers", "external_identifier", 0, 100),
            ],
        ),
        shape(
            "shape.company.postal-address",
            "norn.company.postal-address",
            [
                field("streetAddress", "string", 0, 1),
                field("addressLocality", "string", 1, 1),
                field("addressRegion", "string", 0, 1),
                field("postalCode", "string", 1, 1),
                field("addressCountry", "string", 1, 1),
            ],
        ),
        shape(
            "shape.commerce.product",
            "norn.commerce.product",
            [
                field("name", "string", 1, 1),
                field("description", "string", 0, 1),
                field("sku", "string", 0, 1),
                field("externalIdentifiers", "external_identifier", 0, 100),
                field("category", "category_ref", 0, 100),
                field("categories", "category_ref", 0, 100),
                field("additionalProperty", "external_identifier", 0, 100),
                field("offers", "term_ref", 0, 100, "norn.commerce.offer"),
                field("measurementUnit", "term_ref", 0, 1, "norn.measure.unit"),
            ],
        ),
        shape(
            "shape.commerce.product-variant",
            "norn.commerce.product-variant",
            [
                field("name", "string", 1, 1),
                field("parentProduct", "term_ref", 1, 1, "norn.commerce.product"),
                field("sku", "string", 0, 1),
                field("externalIdentifiers", "external_identifier", 0, 100),
                field("category", "category_ref", 0, 100),
                field("categories", "category_ref", 0, 100),
                field("additionalProperty", "external_identifier", 0, 100),
            ],
        ),
        shape(
            "shape.commerce.offer",
            "norn.commerce.offer",
            [
                field("itemOffered", "term_ref", 1, 1, "norn.commerce.product"),
                field("price", "decimal", 1, 1),
                field("priceCurrency", "string", 1, 1),
                field("availability", "string", 0, 1),
                field("seller", "term_ref", 0, 1, "norn.company.party"),
                field("additionalProperty", "external_identifier", 0, 100),
            ],
        ),
        shape(
            "shape.commerce.order",
            "norn.commerce.order",
            [
                field("orderNumber", "string", 1, 1),
                field("orderDate", "date_time", 1, 1),
                field("orderStatus", "string", 1, 1),
                field("customer", "term_ref", 0, 1, "norn.company.party"),
                field("merchant", "term_ref", 0, 1, "norn.company.party"),
                field("acceptedOffer", "term_ref", 0, 1, "norn.commerce.offer"),
                field("billingAddress", "term_ref", 0, 1, "norn.company.postal-address"),
                field("shippingAddress", "term_ref", 0, 1, "norn.company.postal-address"),
                field("lineItems", "term_ref", 1, 100, "norn.commerce.order-item"),
            ],
        ),
        shape(
            "shape.commerce.order-item",
            "norn.commerce.order-item",
            [
                field("orderItemNumber", "string", 0, 1),
                field("orderItemStatus", "string", 0, 1),
                field("orderedProduct", "term_ref", 1, 1, "norn.commerce.product"),
                field("orderQuantity", "decimal", 1, 1),
                field("additionalProperty", "external_identifier", 0, 100),
            ],
        ),
        shape(
            "shape.logistics.trade-item",
            "norn.logistics.trade-item",
            [
                field("variantRef", "term_ref", 1, 1, "norn.commerce.product-variant"),
                field("packagingLevel", "string", 1, 1),
                field("externalIdentifiers", "external_identifier", 1, 100),
            ],
        ),
        shape(
            "shape.taxonomy.category",
            "norn.taxonomy.category",
            [
                field("code", "string", 1, 1),
                field("label", "string", 1, 1),
                field("parentCategory", "category_ref", 0, 1),
            ],
        ),
        shape(
            "shape.identity.external-identifier",
            "norn.identity.external-identifier",
            [
                field("scheme", "string", 1, 1),
                field("value", "string", 1, 1),
                field("issuer", "string", 0, 1),
            ],
        ),
        shape(
            "shape.evidence.provenance-record",
            "norn.evidence.provenance-record",
            [
                field("sourceRef", "string", 1, 1),
                field("observedAt", "date_time", 1, 1),
                field("evidenceRefs", "iri", 1, 100),
            ],
        ),
    ]


def build_pack() -> dict[str, Any]:
    categories = load_json(PACK_DIR / f"shopify-categories.{TAXONOMY_VERSION}.json")
    attributes = load_json(PACK_DIR / f"shopify-attributes.{TAXONOMY_VERSION}.json")
    return {
        "irVersion": "0.1",
        "artifactId": "norn.ontology-pack.company-commerce",
        "kind": "business_pack",
        "namespace": "norn.ontology",
        "name": "Company Commerce Ontology v0",
        "version": "0.1.0",
        "lifecycle": "proposed",
        "scope": {},
        "dependencies": [],
        "provenance": {
            "source": "system",
            "sourcePath": "verification/ontology/packs/company-commerce-v0/pack.json",
            "createdAt": f"{RETRIEVED_DATE}T00:00:00Z",
        },
        "payload": {
            "packId": "norn.ontology-pack.company-commerce",
            "packVersion": "0.1",
            "packKind": "ontology",
            "sources": build_sources(),
            "terms": build_terms(),
            "mappings": build_mappings(categories, attributes),
            "shapes": build_shapes(),
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=PACK_DIR / "pack.json")
    args = parser.parse_args()
    args.output.write_text(json.dumps(build_pack(), ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
