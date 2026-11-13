import { Norn } from "@norn/authoring"
import { Agent, AgentRole, Resource, Commitment, EconomicEvent, LedgerEffect } from "../../core/src/physics.ts"
import { Catalog, Item, Party, PartyRole, Holding, OrderLine, Delivery, Posting, Settlement, SettlementObservation, ISO_4217_CODE_SET } from "../canon/records.ts"

export const Source = Norn.sourceSchema({
  id: "odoo.three.source",
  title: "Synthetic odoo snapshot and incremental profile",
  version: "0.1.0",
  source: {system: "synthetic-odoo", instance: "demo", version: "odoo-three/1"},
  observedAt: "2026-11-02T00:00:00Z",
  adapter: {name: "synthetic-record-replay", version: "0.1.0"},
  rawMetadata: {uri: "registry/packs/odoo/synthetic.json", sha256: "4b8f939a1266c787769606584c57fc3f06e6652e4ac0b7fa05f383a576043d46"},
  datasets: [
    {id: "product.product", nativeName: "Synthetic product.product", kind: "document"},
    {id: "stock.quant", nativeName: "Synthetic stock.quant", kind: "document"},
    {id: "res.partner", nativeName: "Synthetic res.partner", kind: "document"},
    {id: "sale.order.line", nativeName: "Synthetic sale.order.line", kind: "document"},
    {id: "stock.move", nativeName: "Synthetic stock.move", kind: "document"},
    {id: "account.move.line", nativeName: "Synthetic account.move.line", kind: "document"},
    {id: "account.payment", nativeName: "Synthetic account.payment", kind: "document"}
  ],
  fields: [
    {dataset: "product.product", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "product.product", name: "x_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "product.product", name: "code", ordinal: 2, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "product.product", name: "label", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "product.product", name: "sku", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "product.product", name: "price", ordinal: 5, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "x_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "resource_code", ordinal: 2, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "quantity", ordinal: 3, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "unit", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "location", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "item_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "owner_ref", ordinal: 7, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "custodian_ref", ordinal: 8, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "stock_cost", ordinal: 9, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "cost_currency", ordinal: 10, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "res.partner", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "res.partner", name: "x_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "res.partner", name: "label", ordinal: 2, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "res.partner", name: "role", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sale.order.line", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sale.order.line", name: "x_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sale.order.line", name: "quantity", ordinal: 2, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "sale.order.line", name: "unit", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sale.order.line", name: "due_at", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sale.order.line", name: "provider_ref", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sale.order.line", name: "receiver_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "sale.order.line", name: "holding_ref", ordinal: 7, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.move", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.move", name: "x_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.move", name: "quantity", ordinal: 2, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "stock.move", name: "unit", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.move", name: "occurred_at", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.move", name: "provider_ref", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.move", name: "receiver_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.move", name: "holding_ref", ordinal: 7, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.move", name: "order_ref", ordinal: 8, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.move.line", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.move.line", name: "x_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.move.line", name: "account_code", ordinal: 2, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.move.line", name: "amount", ordinal: 3, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "account.move.line", name: "currency", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.move.line", name: "book", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.move.line", name: "account_role", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.move.line", name: "event_ref", ordinal: 7, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.payment", name: "id", ordinal: 0, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.payment", name: "x_norn_ref", ordinal: 1, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.payment", name: "amount", ordinal: 2, nativeType: "SyntheticDecimal", typeCandidate: {type: "decimal", origin: "declared"}, required: true},
    {dataset: "account.payment", name: "currency", ordinal: 3, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.payment", name: "kind", ordinal: 4, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.payment", name: "payer_ref", ordinal: 5, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.payment", name: "payee_ref", ordinal: 6, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "valuation_method", ordinal: 11, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.quant", name: "write_date", ordinal: 12, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "stock.move", name: "posting_state", ordinal: 9, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "account.move.line", name: "posting_state", ordinal: 8, nativeType: "SyntheticText", typeCandidate: {type: "string", origin: "declared"}, required: true}
  ],
  codeSets: [
    {id: "holding_unit", dataset: "stock.quant", field: "unit", origin: "declared", values: [{value: "g"}]},
    {id: "order_unit", dataset: "sale.order.line", field: "unit", origin: "declared", values: [{value: "g"}]},
    {id: "delivery_unit", dataset: "stock.move", field: "unit", origin: "declared", values: [{value: "g"}]},
    {id: "settlement_kind", dataset: "account.payment", field: "kind", origin: "declared", values: [{value: "payment"}, {value: "refund"}]},
    {id: "holding_valuation_method", dataset: "stock.quant", field: "valuation_method", origin: "declared", values: [{value: "moving_average"}]},
    {id: "holding_cost_currency", dataset: "stock.quant", field: "cost_currency", origin: "declared", values: ISO_4217_CODE_SET},
    {id: "delivery_posting_state", dataset: "stock.move", field: "posting_state", origin: "declared", values: [{value: "posted"}]},
    {id: "posting_posting_state", dataset: "account.move.line", field: "posting_state", origin: "declared", values: [{value: "posted"}]},
    {id: "posting_currency", dataset: "account.move.line", field: "currency", origin: "declared", values: ISO_4217_CODE_SET},
    {id: "posting_account_role", dataset: "account.move.line", field: "account_role", origin: "declared", values: [{value: "cogs"}, {value: "inventory"}]},
    {id: "posting_book", dataset: "account.move.line", field: "book", origin: "declared", values: [{value: "demo/stock-usd"}]},
    {id: "settlement_currency", dataset: "account.payment", field: "currency", origin: "declared", values: ISO_4217_CODE_SET}
  ],
  constraints: [
    {id: "source_id_0", kind: "primary_key", dataset: "product.product", fields: ["id"], origin: "declared"},
    {id: "source_id_1", kind: "primary_key", dataset: "stock.quant", fields: ["id"], origin: "declared"},
    {id: "source_id_2", kind: "primary_key", dataset: "res.partner", fields: ["id"], origin: "declared"},
    {id: "source_id_3", kind: "primary_key", dataset: "sale.order.line", fields: ["id"], origin: "declared"},
    {id: "source_id_4", kind: "primary_key", dataset: "stock.move", fields: ["id"], origin: "declared"},
    {id: "source_id_5", kind: "primary_key", dataset: "account.move.line", fields: ["id"], origin: "declared"},
    {id: "source_id_6", kind: "primary_key", dataset: "account.payment", fields: ["id"], origin: "declared"}
  ]
})

export const Mapping = Norn.mapping({
  id: "odoo.three.mapping",
  title: "odoo synthetic observations to the REA canon",
  version: "0.1.0",
  source: {schemaId: "odoo.three.source", version: "0.1.0", sha256: "6472d633da08e9dadc2251228e21269964367122047b5c8bc79041680ddeee75"},
  sourceContract: {kind: "generic"},
  interfaces: [Catalog, Resource, Agent, AgentRole, Commitment, EconomicEvent, LedgerEffect, Settlement],
  maps: [
    {
      id: "item",
      source: {dataset: "product.product"},
      target: {type: Item, interface: Catalog},
      subject: "input.x_norn_ref",
      sourceRecord: "'odoo/demo/product.product/' + input.id",
      properties: [{property: "id", value: "input.x_norn_ref"}, {property: "code", value: "input.code"}, {property: "label", value: "input.label"}],
      ignoredFields: [{field: "sku", reason: "Alias only; duplicate SKUs do not establish identity."}, {field: "price", reason: "Sales price is not stock cost, valuation or a ledger effect."}]
    },
    {
      id: "holding",
      source: {dataset: "stock.quant"},
      target: {type: Holding, interface: Resource},
      subject: "input.x_norn_ref",
      sourceRecord: "'odoo/demo/stock.quant/' + input.id",
      properties: [
        {property: "resource_code", value: "input.resource_code"},
        {property: "quantity", value: "input.quantity"},
        {property: "observed_at", value: "timestamp(input.write_date)"},
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
      source: {dataset: "res.partner"},
      target: {type: Party, interface: Agent},
      subject: "input.x_norn_ref",
      sourceRecord: "'odoo/demo/res.partner/' + input.id",
      properties: [{property: "display_name", value: "input.label"}, {property: "kind", value: "'organization'"}],
      ignoredFields: [{field: "role", reason: "Mapped separately as an AgentRole; a customer role is not an Agent subtype."}]
    },
    {
      id: "party_role",
      source: {dataset: "res.partner"},
      target: {type: PartyRole, interface: AgentRole},
      subject: "input.x_norn_ref + '/role/' + input.role",
      sourceRecord: "'odoo/demo/res.partner/' + input.id",
      properties: [{property: "role", value: "input.role"}, {property: "valid_from", value: "null"}, {property: "valid_to", value: "null"}],
      links: [{link: "agent", targetMap: "party", key: "input.x_norn_ref"}],
      gaps: [{member: "counterparty", reason: "not_in_source", note: "The fixture role is scoped to the demo organisation, not an inferred bilateral legal relationship."}],
      ignoredFields: [{field: "label", reason: "Agent label is mapped by party."}]
    },
    {
      id: "order",
      source: {dataset: "sale.order.line"},
      target: {type: OrderLine, interface: Commitment},
      subject: "input.x_norn_ref",
      sourceRecord: "'odoo/demo/sale.order.line/' + input.id",
      properties: [{property: "quantity", value: "input.quantity"}, {property: "unit", value: "input.unit"}, {property: "currency", value: "null"}, {property: "status", value: "'promised'"}, {property: "due_at", value: "timestamp(input.due_at)"}],
      links: [{link: "provider", targetMap: "party", key: "input.provider_ref"}, {link: "receiver", targetMap: "party", key: "input.receiver_ref"}, {link: "resource", targetMap: "holding", key: "input.holding_ref"}]
    },
    {
      id: "delivery",
      source: {dataset: "stock.move"},
      target: {type: Delivery, interface: EconomicEvent},
      subject: "input.x_norn_ref",
      sourceRecord: "'odoo/demo/stock.move/' + input.id",
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
      source: {dataset: "account.move.line"},
      target: {type: Posting, interface: LedgerEffect},
      subject: "input.x_norn_ref",
      sourceRecord: "'odoo/demo/account.move.line/' + input.id",
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
      source: {dataset: "account.payment"},
      target: {type: SettlementObservation, interface: Settlement},
      subject: "input.x_norn_ref",
      sourceRecord: "'odoo/demo/account.payment/' + input.id",
      properties: [{property: "amount", value: "input.amount"}, {property: "currency", value: "input.currency"}, {property: "kind", value: "input.kind"}],
      links: [{link: "payer", targetMap: "party", key: "input.payer_ref"}, {link: "payee", targetMap: "party", key: "input.payee_ref"}],
      gaps: [{member: "event", reason: "not_in_source", note: "Payment/refund report alone does not prove cash resource movement, settlement finality or balanced GL posting."}]
    }
  ]
})
