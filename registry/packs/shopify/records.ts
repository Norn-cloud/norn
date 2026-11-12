import { Norn } from "@norn/authoring"
import { Agent, AgentRole, Resource, Commitment, EconomicEvent } from "../../core/src/physics.ts"
import { Catalog, Item, Party, PartyRole, Holding, OrderLine, Delivery, Settlement, SettlementObservation } from "../canon/records.ts"

export const Source = Norn.sourceSchema({
  id: "shopify.three.source",
  title: "Synthetic shopify snapshot and incremental profile",
  version: "0.1.0",
  source: {system: "synthetic-shopify", instance: "demo", version: "shopify-three/1"},
  observedAt: "2026-11-02T00:00:00Z",
  adapter: {name: "synthetic-record-replay", version: "0.1.0"},
  rawMetadata: {uri: "registry/packs/shopify/synthetic.json", sha256: "6669d3251acb3d634d5b827a75559c4e1a89ed66abd8671e80e08feb4a5de59b"},
  datasets: [
    {id: "ProductVariant", nativeName: "Synthetic ProductVariant", kind: "document"},
    {id: "InventoryLevel", nativeName: "Synthetic InventoryLevel", kind: "document"},
    {id: "Customer", nativeName: "Synthetic Customer", kind: "document"},
    {id: "OrderLine", nativeName: "Synthetic OrderLine", kind: "document"},
    {id: "FulfillmentLine", nativeName: "Synthetic FulfillmentLine", kind: "document"},
    {id: "Transaction", nativeName: "Synthetic Transaction", kind: "document"}
  ],
  fields: [
    {dataset: "ProductVariant", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "ProductVariant", name: "metafield_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "ProductVariant", name: "code", ordinal: 2, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "ProductVariant", name: "label", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "ProductVariant", name: "sku", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "ProductVariant", name: "price", ordinal: 5, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "metafield_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "resource_code", ordinal: 2, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "quantity", ordinal: 3, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "unit", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "location", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "item_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "owner_ref", ordinal: 7, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "custodian_ref", ordinal: 8, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "InventoryLevel", name: "updated_at", ordinal: 9, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Customer", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Customer", name: "metafield_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Customer", name: "label", ordinal: 2, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Customer", name: "role", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "OrderLine", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "OrderLine", name: "metafield_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "OrderLine", name: "quantity", ordinal: 2, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "OrderLine", name: "unit", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "OrderLine", name: "due_at", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "OrderLine", name: "provider_ref", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "OrderLine", name: "receiver_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "OrderLine", name: "holding_ref", ordinal: 7, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "FulfillmentLine", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "FulfillmentLine", name: "metafield_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "FulfillmentLine", name: "quantity", ordinal: 2, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "FulfillmentLine", name: "unit", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "FulfillmentLine", name: "occurred_at", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "FulfillmentLine", name: "provider_ref", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "FulfillmentLine", name: "receiver_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "FulfillmentLine", name: "holding_ref", ordinal: 7, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "FulfillmentLine", name: "order_ref", ordinal: 8, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Transaction", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Transaction", name: "metafield_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Transaction", name: "amount", ordinal: 2, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "Transaction", name: "currency", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Transaction", name: "kind", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Transaction", name: "payer_ref", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Transaction", name: "payee_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "FulfillmentLine", name: "posting_state", ordinal: 9, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true}
  ],
  codeSets: [
    {id: "holding_unit", dataset: "InventoryLevel", field: "unit", origin: "declared", values: [{value: "g"}]},
    {id: "order_unit", dataset: "OrderLine", field: "unit", origin: "declared", values: [{value: "g"}]},
    {id: "delivery_unit", dataset: "FulfillmentLine", field: "unit", origin: "declared", values: [{value: "g"}]},
    {id: "settlement_kind", dataset: "Transaction", field: "kind", origin: "declared", values: [{value: "payment"}, {value: "refund"}]},
    {id: "delivery_posting_state", dataset: "FulfillmentLine", field: "posting_state", origin: "declared", values: [{value: "posted"}]},
    {id: "settlement_currency", dataset: "Transaction", field: "currency", origin: "declared", values: [{value: "USD"}]}
  ],
  constraints: [
    {id: "source_id_0", kind: "primary_key", dataset: "ProductVariant", fields: ["id"], origin: "declared"},
    {id: "source_id_1", kind: "primary_key", dataset: "InventoryLevel", fields: ["id"], origin: "declared"},
    {id: "source_id_2", kind: "primary_key", dataset: "Customer", fields: ["id"], origin: "declared"},
    {id: "source_id_3", kind: "primary_key", dataset: "OrderLine", fields: ["id"], origin: "declared"},
    {id: "source_id_4", kind: "primary_key", dataset: "FulfillmentLine", fields: ["id"], origin: "declared"},
    {id: "source_id_6", kind: "primary_key", dataset: "Transaction", fields: ["id"], origin: "declared"}
  ]
})

export const Mapping = Norn.mapping({
  id: "shopify.three.mapping",
  title: "shopify synthetic observations to the REA canon",
  version: "0.1.0",
  source: {schemaId: "shopify.three.source", version: "0.1.0", sha256: "0f3b9172611997d6765978334a2a6b7cdd533930299d2194a92bb137dcee5f1d"},
  sourceContract: {kind: "generic"},
  interfaces: [Catalog, Resource, Agent, AgentRole, Commitment, EconomicEvent, Settlement],
  maps: [
    {
      id: "item",
      source: {dataset: "ProductVariant"},
      target: {type: Item, interface: Catalog},
      subject: "input.metafield_norn_ref",
      sourceRecord: "'shopify/demo/ProductVariant/' + input.id",
      properties: [{property: "id", value: "input.metafield_norn_ref"}, {property: "code", value: "input.code"}, {property: "label", value: "input.label"}],
      ignoredFields: [{field: "sku", reason: "Alias only; duplicate SKUs do not establish identity."}, {field: "price", reason: "Sales price is not stock cost, valuation or a ledger effect."}]
    },
    {
      id: "holding",
      source: {dataset: "InventoryLevel"},
      target: {type: Holding, interface: Resource},
      subject: "input.metafield_norn_ref",
      sourceRecord: "'shopify/demo/InventoryLevel/' + input.id",
      properties: [
        {property: "resource_code", value: "input.resource_code"},
        {property: "quantity", value: "input.quantity"},
        {property: "observed_at", value: "timestamp(input.updated_at)"},
        {property: "unit", value: "input.unit"},
        {property: "currency", value: "null"},
        {property: "valuation_method", value: "null"},
        {property: "location", value: "input.location"},
        {property: "quantity_basis", value: "'available'"}
      ],
      links: [{link: "item", targetMap: "item", key: "input.item_ref"}, {link: "owner", targetMap: "party", key: "input.owner_ref"}, {link: "custodian", targetMap: "party", key: "input.custodian_ref"}],
      gaps: [
        {member: "stock_cost", reason: "not_in_source", note: "Shopify availability supplies neither original cost, cost currency nor a valuation method. Authoritative stock valuation coverage is incomplete."},
        {member: "cost_currency", reason: "not_in_source", note: "Shopify availability supplies neither original cost, cost currency nor a valuation method. Authoritative stock valuation coverage is incomplete."}
      ]
    },
    {
      id: "party",
      source: {dataset: "Customer"},
      target: {type: Party, interface: Agent},
      subject: "input.metafield_norn_ref",
      sourceRecord: "'shopify/demo/Customer/' + input.id",
      properties: [{property: "display_name", value: "input.label"}, {property: "kind", value: "'organization'"}],
      ignoredFields: [{field: "role", reason: "Mapped separately as an AgentRole; a customer role is not an Agent subtype."}]
    },
    {
      id: "party_role",
      source: {dataset: "Customer"},
      target: {type: PartyRole, interface: AgentRole},
      subject: "input.metafield_norn_ref + '/role/' + input.role",
      sourceRecord: "'shopify/demo/Customer/' + input.id",
      properties: [{property: "role", value: "input.role"}, {property: "valid_from", value: "null"}, {property: "valid_to", value: "null"}],
      links: [{link: "agent", targetMap: "party", key: "input.metafield_norn_ref"}],
      gaps: [{member: "counterparty", reason: "not_in_source", note: "The fixture role is scoped to the demo organisation, not an inferred bilateral legal relationship."}],
      ignoredFields: [{field: "label", reason: "Agent label is mapped by party."}]
    },
    {
      id: "order",
      source: {dataset: "OrderLine"},
      target: {type: OrderLine, interface: Commitment},
      subject: "input.metafield_norn_ref",
      sourceRecord: "'shopify/demo/OrderLine/' + input.id",
      properties: [{property: "quantity", value: "input.quantity"}, {property: "unit", value: "input.unit"}, {property: "currency", value: "null"}, {property: "status", value: "'promised'"}, {property: "due_at", value: "timestamp(input.due_at)"}],
      links: [{link: "provider", targetMap: "party", key: "input.provider_ref"}, {link: "receiver", targetMap: "party", key: "input.receiver_ref"}, {link: "resource", targetMap: "holding", key: "input.holding_ref"}]
    },
    {
      id: "delivery",
      source: {dataset: "FulfillmentLine"},
      target: {type: Delivery, interface: EconomicEvent},
      subject: "input.metafield_norn_ref",
      sourceRecord: "'shopify/demo/FulfillmentLine/' + input.id",
      properties: [
        {property: "quantity", value: "input.quantity"},
        {property: "unit", value: "input.unit"},
        {property: "currency", value: "null"},
        {property: "action", value: "'transfer_ownership'"},
        {property: "occurred_at", value: "timestamp(input.occurred_at)"}
      ],
      gaps: [{member: "reverses", reason: "not_in_source", note: "Only completed forward deliveries; returns and corrections require explicit original-event evidence."}],
      links: [
        {link: "provider", targetMap: "party", key: "input.provider_ref"},
        {link: "receiver", targetMap: "party", key: "input.receiver_ref"},
        {link: "resource", targetMap: "holding", key: "input.holding_ref"},
        {link: "fulfils", targetMap: "order", key: "input.order_ref"}
      ],
      ignoredFields: [{field: "posting_state", reason: "Only the explicitly posted synthetic export profile is admitted by its closed source code set."}]
    },
    {
      id: "settlement",
      source: {dataset: "Transaction"},
      target: {type: SettlementObservation, interface: Settlement},
      subject: "input.metafield_norn_ref",
      sourceRecord: "'shopify/demo/Transaction/' + input.id",
      properties: [{property: "amount", value: "input.amount"}, {property: "currency", value: "input.currency"}, {property: "kind", value: "input.kind"}],
      links: [{link: "payer", targetMap: "party", key: "input.payer_ref"}, {link: "payee", targetMap: "party", key: "input.payee_ref"}],
      gaps: [{member: "event", reason: "not_in_source", note: "Payment/refund report alone does not prove cash resource movement, settlement finality or balanced GL posting."}]
    }
  ]
})
