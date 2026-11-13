import { Norn } from "@norn/authoring"
import { Agent, AgentRole, Resource, Commitment, EconomicEvent, LedgerEffect } from "../../core/src/physics.ts"
import { Catalog, Item, Party, PartyRole, Holding, OrderLine, Delivery, Posting, Settlement, SettlementObservation, ISO_4217_CODE_SET } from "../canon/records.ts"

export const Source = Norn.sourceSchema({
  id: "erpnext.three.source",
  title: "Synthetic erpnext snapshot and incremental profile",
  version: "0.1.0",
  source: {system: "synthetic-erpnext", instance: "demo", version: "erpnext-three/1"},
  observedAt: "2026-11-02T00:00:00Z",
  adapter: {name: "synthetic-record-replay", version: "0.1.0"},
  rawMetadata: {uri: "registry/packs/erpnext/synthetic.json", sha256: "c6be2331a6bfdb02e41db56d8da20433662b69c972f1e4ee49caecb0ccb29f6c"},
  datasets: [
    {id: "Item", nativeName: "Synthetic Item", kind: "document"},
    {id: "Warehouse Lot Balance", nativeName: "Synthetic Warehouse Lot Balance", kind: "document"},
    {id: "Party", nativeName: "Synthetic Party", kind: "document"},
    {id: "Sales Order Item", nativeName: "Synthetic Sales Order Item", kind: "document"},
    {id: "Delivery Note Item", nativeName: "Synthetic Delivery Note Item", kind: "document"},
    {id: "GL Entry", nativeName: "Synthetic GL Entry", kind: "document"},
    {id: "Payment Entry", nativeName: "Synthetic Payment Entry", kind: "document"}
  ],
  fields: [
    {dataset: "Item", name: "name", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "custom_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "code", ordinal: 2, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "label", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "sku", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "price", ordinal: 5, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "name", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "custom_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "resource_code", ordinal: 2, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "quantity", ordinal: 3, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "unit", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "location", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "item_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "owner_ref", ordinal: 7, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "custodian_ref", ordinal: 8, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "stock_cost", ordinal: 9, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "cost_currency", ordinal: 10, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Party", name: "name", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Party", name: "custom_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Party", name: "label", ordinal: 2, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Party", name: "role", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Sales Order Item", name: "name", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Sales Order Item", name: "custom_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Sales Order Item", name: "quantity", ordinal: 2, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "Sales Order Item", name: "unit", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Sales Order Item", name: "due_at", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Sales Order Item", name: "provider_ref", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Sales Order Item", name: "receiver_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Sales Order Item", name: "holding_ref", ordinal: 7, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Delivery Note Item", name: "name", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Delivery Note Item", name: "custom_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Delivery Note Item", name: "quantity", ordinal: 2, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "Delivery Note Item", name: "unit", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Delivery Note Item", name: "occurred_at", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Delivery Note Item", name: "provider_ref", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Delivery Note Item", name: "receiver_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Delivery Note Item", name: "holding_ref", ordinal: 7, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Delivery Note Item", name: "order_ref", ordinal: 8, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "GL Entry", name: "name", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "GL Entry", name: "custom_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "GL Entry", name: "account_code", ordinal: 2, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "GL Entry", name: "amount", ordinal: 3, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "GL Entry", name: "currency", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "GL Entry", name: "book", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "GL Entry", name: "account_role", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "GL Entry", name: "event_ref", ordinal: 7, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Payment Entry", name: "name", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Payment Entry", name: "custom_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Payment Entry", name: "amount", ordinal: 2, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "Payment Entry", name: "currency", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Payment Entry", name: "kind", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Payment Entry", name: "payer_ref", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Payment Entry", name: "payee_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "valuation_method", ordinal: 11, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Warehouse Lot Balance", name: "modified", ordinal: 12, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Delivery Note Item", name: "posting_state", ordinal: 9, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "GL Entry", name: "posting_state", ordinal: 8, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true}
  ],
  codeSets: [
    {id: "holding_unit", dataset: "Warehouse Lot Balance", field: "unit", origin: "declared", values: [{value: "g"}]},
    {id: "order_unit", dataset: "Sales Order Item", field: "unit", origin: "declared", values: [{value: "g"}]},
    {id: "delivery_unit", dataset: "Delivery Note Item", field: "unit", origin: "declared", values: [{value: "g"}]},
    {id: "settlement_kind", dataset: "Payment Entry", field: "kind", origin: "declared", values: [{value: "payment"}, {value: "refund"}]},
    {id: "holding_valuation_method", dataset: "Warehouse Lot Balance", field: "valuation_method", origin: "declared", values: [{value: "moving_average"}]},
    {id: "holding_cost_currency", dataset: "Warehouse Lot Balance", field: "cost_currency", origin: "declared", values: ISO_4217_CODE_SET},
    {id: "delivery_posting_state", dataset: "Delivery Note Item", field: "posting_state", origin: "declared", values: [{value: "posted"}]},
    {id: "posting_posting_state", dataset: "GL Entry", field: "posting_state", origin: "declared", values: [{value: "posted"}]},
    {id: "posting_currency", dataset: "GL Entry", field: "currency", origin: "declared", values: ISO_4217_CODE_SET},
    {id: "posting_account_role", dataset: "GL Entry", field: "account_role", origin: "declared", values: [{value: "cogs"}, {value: "inventory"}]},
    {id: "posting_book", dataset: "GL Entry", field: "book", origin: "declared", values: [{value: "demo/stock-usd"}]},
    {id: "settlement_currency", dataset: "Payment Entry", field: "currency", origin: "declared", values: ISO_4217_CODE_SET}
  ],
  constraints: [
    {id: "source_id_0", kind: "primary_key", dataset: "Item", fields: ["name"], origin: "declared"},
    {id: "source_id_1", kind: "primary_key", dataset: "Warehouse Lot Balance", fields: ["name"], origin: "declared"},
    {id: "source_id_2", kind: "primary_key", dataset: "Party", fields: ["name"], origin: "declared"},
    {id: "source_id_3", kind: "primary_key", dataset: "Sales Order Item", fields: ["name"], origin: "declared"},
    {id: "source_id_4", kind: "primary_key", dataset: "Delivery Note Item", fields: ["name"], origin: "declared"},
    {id: "source_id_5", kind: "primary_key", dataset: "GL Entry", fields: ["name"], origin: "declared"},
    {id: "source_id_6", kind: "primary_key", dataset: "Payment Entry", fields: ["name"], origin: "declared"}
  ]
})

export const Mapping = Norn.mapping({
  id: "erpnext.three.mapping",
  title: "erpnext synthetic observations to the REA canon",
  version: "0.1.0",
  source: {schemaId: "erpnext.three.source", version: "0.1.0", sha256: "eb61ffb42f8a4d6b0ee9f3f153fa647e7a03779c96622ba1f6568e16bc234574"},
  sourceContract: {kind: "generic"},
  interfaces: [Catalog, Resource, Agent, AgentRole, Commitment, EconomicEvent, LedgerEffect, Settlement],
  maps: [
    {
      id: "item",
      source: {dataset: "Item"},
      target: {type: Item, interface: Catalog},
      subject: "input.custom_norn_ref",
      sourceRecord: "'erpnext/demo/Item/' + input.name",
      properties: [{property: "id", value: "input.custom_norn_ref"}, {property: "code", value: "input.code"}, {property: "label", value: "input.label"}],
      ignoredFields: [{field: "sku", reason: "Alias only; duplicate SKUs do not establish identity."}, {field: "price", reason: "Sales price is not stock cost, valuation or a ledger effect."}]
    },
    {
      id: "holding",
      source: {dataset: "Warehouse Lot Balance"},
      target: {type: Holding, interface: Resource},
      subject: "input.custom_norn_ref",
      sourceRecord: "'erpnext/demo/Warehouse Lot Balance/' + input.name",
      properties: [
        {property: "resource_code", value: "input.resource_code"},
        {property: "quantity", value: "input.quantity"},
        {property: "observed_at", value: "timestamp(input.modified)"},
        {property: "unit", value: "input.unit"},
        {property: "currency", value: "null"},
        {property: "valuation_method", value: "input.valuation_method"},
        {property: "location", value: "input.location"},
        {property: "quantity_basis", value: "'on_hand'"},
        {property: "stock_cost", value: "input.stock_cost"},
        {property: "cost_currency", value: "input.cost_currency"}
      ],
      links: [{link: "item", targetMap: "item", key: "input.item_ref"}, {link: "owner", targetMap: "party", key: "input.owner_ref"}, {link: "custodian", targetMap: "party", key: "input.custodian_ref"}]
    },
    {
      id: "party",
      source: {dataset: "Party"},
      target: {type: Party, interface: Agent},
      subject: "input.custom_norn_ref",
      sourceRecord: "'erpnext/demo/Party/' + input.name",
      properties: [{property: "display_name", value: "input.label"}, {property: "kind", value: "'organization'"}],
      ignoredFields: [{field: "role", reason: "Mapped separately as an AgentRole; a customer role is not an Agent subtype."}]
    },
    {
      id: "party_role",
      source: {dataset: "Party"},
      target: {type: PartyRole, interface: AgentRole},
      subject: "input.custom_norn_ref + '/role/' + input.role",
      sourceRecord: "'erpnext/demo/Party/' + input.name",
      properties: [{property: "role", value: "input.role"}, {property: "valid_from", value: "null"}, {property: "valid_to", value: "null"}],
      links: [{link: "agent", targetMap: "party", key: "input.custom_norn_ref"}],
      gaps: [{member: "counterparty", reason: "not_in_source", note: "The fixture role is scoped to the demo organisation, not an inferred bilateral legal relationship."}],
      ignoredFields: [{field: "label", reason: "Agent label is mapped by party."}]
    },
    {
      id: "order",
      source: {dataset: "Sales Order Item"},
      target: {type: OrderLine, interface: Commitment},
      subject: "input.custom_norn_ref",
      sourceRecord: "'erpnext/demo/Sales Order Item/' + input.name",
      properties: [{property: "quantity", value: "input.quantity"}, {property: "unit", value: "input.unit"}, {property: "currency", value: "null"}, {property: "status", value: "'promised'"}, {property: "due_at", value: "timestamp(input.due_at)"}],
      links: [{link: "provider", targetMap: "party", key: "input.provider_ref"}, {link: "receiver", targetMap: "party", key: "input.receiver_ref"}, {link: "resource", targetMap: "holding", key: "input.holding_ref"}]
    },
    {
      id: "delivery",
      source: {dataset: "Delivery Note Item"},
      target: {type: Delivery, interface: EconomicEvent},
      subject: "input.custom_norn_ref",
      sourceRecord: "'erpnext/demo/Delivery Note Item/' + input.name",
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
      id: "posting",
      source: {dataset: "GL Entry"},
      target: {type: Posting, interface: LedgerEffect},
      subject: "input.custom_norn_ref",
      sourceRecord: "'erpnext/demo/GL Entry/' + input.name",
      properties: [
        {property: "account_code", value: "input.account_code"},
        {property: "amount", value: "input.amount"},
        {property: "currency", value: "input.currency"},
        {property: "book", value: "input.book"},
        {property: "account_role", value: "input.account_role"}
      ],
      links: [{link: "event", targetMap: "delivery", key: "input.event_ref"}],
      ignoredFields: [{field: "posting_state", reason: "Only the explicitly posted synthetic export profile is admitted by its closed source code set."}]
    },
    {
      id: "settlement",
      source: {dataset: "Payment Entry"},
      target: {type: SettlementObservation, interface: Settlement},
      subject: "input.custom_norn_ref",
      sourceRecord: "'erpnext/demo/Payment Entry/' + input.name",
      properties: [{property: "amount", value: "input.amount"}, {property: "currency", value: "input.currency"}, {property: "kind", value: "input.kind"}],
      links: [{link: "payer", targetMap: "party", key: "input.payer_ref"}, {link: "payee", targetMap: "party", key: "input.payee_ref"}],
      gaps: [{member: "event", reason: "not_in_source", note: "Payment/refund report alone does not prove cash resource movement, settlement finality or balanced GL posting."}]
    }
  ]
})
