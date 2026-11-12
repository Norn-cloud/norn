import { Norn } from "@norn/authoring"
import { Agent, AgentRole, Resource, Commitment } from "../../core/src/physics.ts"
import { Catalog, Item, Party, PartyRole, Holding, OrderLine } from "../canon/records.ts"

// A live ERPNext sandbox (#921): the same rows as the synthetic export, read
// through ERPNext's REST API by the generic pull engine (`norn.pull/1`). Native
// doctypes carry native fields; the canonical references are the custom fields
// demo/sandboxes/erpnext/seed.py creates. Feeds read rows after (modified, name)
// in that order, only rows the organisation has referenced; `{{json:…}}` puts the
// position into Frappe's JSON filters as quoted literals. The connection (base
// URL, site and credentials) is server configuration; this pack names only the
// secret ref.

export const SandboxSource = Norn.sourceSchema({
  id: "erpnext.sandbox.source",
  title: "ERPNext sandbox: parties, items, lot balances and order lines",
  version: "0.1.0",
  source: {system: "erpnext", instance: "sandbox", version: "erpnext-16"},
  observedAt: "2026-11-12T00:00:00Z",
  adapter: {name: "norn-pull", version: "1"},
  rawMetadata: {uri: "demo/sandboxes/erpnext/seed.py", sha256: "2bd0d0747ae36ab2f010479b9bb159c67dce41adc8eef3b4017aa09d651c9eba"},
  datasets: [
    {id: "Item", nativeName: "Item", kind: "document"},
    {id: "Warehouse Lot Balance", nativeName: "Warehouse Lot Balance (custom DocType)", kind: "document"},
    {id: "Party", nativeName: "Customer", kind: "document"},
    {id: "Sales Order Item", nativeName: "Sales Order Item", kind: "document"}
  ],
  fields: [
    {dataset: "Item", name: "name", ordinal: 0, nativeType: "Data", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {
      dataset: "Item",
      name: "custom_norn_ref",
      ordinal: 1,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {dataset: "Item", name: "code", ordinal: 2, nativeType: "Data", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "label", ordinal: 3, nativeType: "Data", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {dataset: "Item", name: "sku", ordinal: 4, nativeType: "Data", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {
      dataset: "Item",
      name: "price",
      ordinal: 5,
      nativeType: "Currency",
      typeCandidate: {type: "decimal", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "name",
      ordinal: 0,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "custom_norn_ref",
      ordinal: 1,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "resource_code",
      ordinal: 2,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "quantity",
      ordinal: 3,
      nativeType: "Float",
      typeCandidate: {type: "decimal", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "unit",
      ordinal: 4,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "location",
      ordinal: 5,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "item_ref",
      ordinal: 6,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "owner_ref",
      ordinal: 7,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "custodian_ref",
      ordinal: 8,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "stock_cost",
      ordinal: 9,
      nativeType: "Float",
      typeCandidate: {type: "decimal", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "cost_currency",
      ordinal: 10,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "valuation_method",
      ordinal: 11,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Warehouse Lot Balance",
      name: "modified",
      ordinal: 12,
      nativeType: "Datetime",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {dataset: "Party", name: "name", ordinal: 0, nativeType: "Data", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {
      dataset: "Party",
      name: "custom_norn_ref",
      ordinal: 1,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Party",
      name: "label",
      ordinal: 2,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {dataset: "Party", name: "role", ordinal: 3, nativeType: "Data", typeCandidate: {type: "string", origin: "declared"}, required: true},
    {
      dataset: "Sales Order Item",
      name: "name",
      ordinal: 0,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Sales Order Item",
      name: "custom_norn_ref",
      ordinal: 1,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Sales Order Item",
      name: "quantity",
      ordinal: 2,
      nativeType: "Float",
      typeCandidate: {type: "decimal", origin: "declared"},
      required: true
    },
    {
      dataset: "Sales Order Item",
      name: "unit",
      ordinal: 3,
      nativeType: "Link",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Sales Order Item",
      name: "due_at",
      ordinal: 4,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Sales Order Item",
      name: "provider_ref",
      ordinal: 5,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Sales Order Item",
      name: "receiver_ref",
      ordinal: 6,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    },
    {
      dataset: "Sales Order Item",
      name: "holding_ref",
      ordinal: 7,
      nativeType: "Data",
      typeCandidate: {type: "string", origin: "declared"},
      required: true
    }
  ],
  codeSets: [
    {id: "holding_unit", dataset: "Warehouse Lot Balance", field: "unit", origin: "declared", values: [{value: "g"}]},
    {id: "order_unit", dataset: "Sales Order Item", field: "unit", origin: "declared", values: [{value: "g"}]},
    {
      id: "holding_valuation_method",
      dataset: "Warehouse Lot Balance",
      field: "valuation_method",
      origin: "declared",
      values: [{value: "moving_average"}]
    }
  ],
  constraints: [
    {id: "source_id_0", kind: "primary_key", dataset: "Item", fields: ["name"], origin: "declared"},
    {id: "source_id_1", kind: "primary_key", dataset: "Warehouse Lot Balance", fields: ["name"], origin: "declared"},
    {id: "source_id_2", kind: "primary_key", dataset: "Party", fields: ["name"], origin: "declared"},
    {id: "source_id_3", kind: "primary_key", dataset: "Sales Order Item", fields: ["name"], origin: "declared"}
  ]
})

export const SandboxMapping = Norn.mapping({
  id: "erpnext.sandbox.mapping",
  title: "ERPNext sandbox observations to the REA canon",
  version: "0.1.0",
  source: {schemaId: "erpnext.sandbox.source", version: "0.1.0", sha256: "21fcae139208fef42c7e10329a303d80484a1d870ffc169760c1427eb04f1834"},
  sourceContract: {kind: "generic"},
  interfaces: [Catalog, Resource, Agent, AgentRole, Commitment],
  maps: [
    {
      id: "item",
      source: {dataset: "Item"},
      target: {type: Item, interface: Catalog},
      subject: "input.custom_norn_ref",
      sourceRecord: "'erpnext/sandbox/Item/' + input.name",
      properties: [{property: "id", value: "input.custom_norn_ref"}, {property: "code", value: "input.code"}, {property: "label", value: "input.label"}],
      ignoredFields: [
        {field: "sku", reason: "Alias only; duplicate SKUs do not establish identity."},
        {field: "price", reason: "Sales price is not stock cost, valuation or a ledger effect."}
      ]
    },
    {
      id: "holding",
      source: {dataset: "Warehouse Lot Balance"},
      target: {type: Holding, interface: Resource},
      subject: "input.custom_norn_ref",
      sourceRecord: "'erpnext/sandbox/Warehouse Lot Balance/' + input.name",
      properties: [
        {property: "resource_code", value: "input.resource_code"},
        {property: "quantity", value: "input.quantity"},
        // Frappe's `modified` is the site's local time without an offset; the sandbox site runs in UTC.
        {property: "observed_at", value: "timestamp(join(split(input.modified, ' '), 'T') + 'Z')"},
        {property: "unit", value: "input.unit"},
        {property: "currency", value: "null"},
        {property: "valuation_method", value: "input.valuation_method"},
        {property: "location", value: "input.location"},
        {property: "quantity_basis", value: "'on_hand'"},
        {property: "stock_cost", value: "input.stock_cost"},
        {property: "cost_currency", value: "input.cost_currency"}
      ],
      links: [
        {link: "item", targetMap: "item", key: "input.item_ref"},
        {link: "owner", targetMap: "party", key: "input.owner_ref"},
        {link: "custodian", targetMap: "party", key: "input.custodian_ref"}
      ]
    },
    {
      id: "party",
      source: {dataset: "Party"},
      target: {type: Party, interface: Agent},
      subject: "input.custom_norn_ref",
      sourceRecord: "'erpnext/sandbox/Customer/' + input.name",
      properties: [{property: "display_name", value: "input.label"}, {property: "kind", value: "'organization'"}],
      ignoredFields: [{field: "role", reason: "Mapped separately as an AgentRole; a customer role is not an Agent subtype."}]
    },
    {
      id: "party_role",
      source: {dataset: "Party"},
      target: {type: PartyRole, interface: AgentRole},
      subject: "input.custom_norn_ref + '/role/' + input.role",
      sourceRecord: "'erpnext/sandbox/Customer/' + input.name",
      properties: [{property: "role", value: "input.role"}, {property: "valid_from", value: "null"}, {property: "valid_to", value: "null"}],
      links: [{link: "agent", targetMap: "party", key: "input.custom_norn_ref"}],
      gaps: [
        {
          member: "counterparty",
          reason: "not_in_source",
          note: "The role is scoped to the demo organisation, not an inferred bilateral legal relationship."
        }
      ],
      ignoredFields: [{field: "label", reason: "Agent label is mapped by party."}]
    },
    {
      id: "order",
      source: {dataset: "Sales Order Item"},
      target: {type: OrderLine, interface: Commitment},
      subject: "input.custom_norn_ref",
      sourceRecord: "'erpnext/sandbox/Sales Order Item/' + input.name",
      properties: [
        {property: "quantity", value: "input.quantity"},
        {property: "unit", value: "input.unit"},
        {property: "currency", value: "null"},
        {property: "status", value: "'promised'"},
        {property: "due_at", value: "timestamp(input.due_at)"}
      ],
      links: [
        {link: "provider", targetMap: "party", key: "input.provider_ref"},
        {link: "receiver", targetMap: "party", key: "input.receiver_ref"},
        {link: "resource", targetMap: "holding", key: "input.holding_ref"}
      ]
    }
  ]
})

export const SandboxConnector = Norn.connector({
  id: "erpnext.sandbox.connector",
  title: "ERPNext sandbox REST reads",
  version: "0.1.0",
  auth: {
    kind: "api_key",
    authId: "erpnext_sandbox",
    placement: "header",
    name: "Authorization",
    secretRef: "erpnext_sandbox_token",
    prefix: "token ",
    format: {kind: "pair", separator: ":"}
  },
  resources: [
    {
      kind: "delta",
      resourceId: "documents",
      endpoint: "/api/resource/{doctype}",
      sinceParam: "filters",
      updatedAtPath: "/modified",
      itemsPath: "/data"
    }
  ],
  mappings: [],
  pull: {
    source: "erpnext",
    pipeline: "norn-pull/1+erpnext.sandbox.connector@0.1.0",
    feeds: [
      {
        dataset: "Party",
        request: {
          method: "GET",
          path: "/api/resource/Customer",
          query: {
            fields: "[\"name\",\"modified\",\"customer_name\",\"custom_norn_ref\",\"custom_norn_role\"]",
            filters: "[[\"custom_norn_ref\",\"is\",\"set\"],[\"modified\",\">=\",{{json:version}}]]",
            or_filters: "[[\"modified\",\">\",{{json:version}}],[\"name\",\">\",{{json:id}}]]",
            order_by: "modified asc, name asc",
            limit_start: "0",
            limit_page_length: "{{size}}"
          }
        },
        itemsPath: "/data",
        paging: {kind: "keyset", size: 100},
        idPath: "/name",
        versionPath: "/modified",
        fields: {name: "/name", custom_norn_ref: "/custom_norn_ref", label: "/customer_name", role: "/custom_norn_role"},
        initial: {version: "1970-01-01 00:00:00.000000", id: ""}
      },
      {
        dataset: "Item",
        request: {
          method: "GET",
          path: "/api/resource/Item",
          query: {
            fields: "[\"name\",\"modified\",\"item_code\",\"item_name\",\"custom_sku\",\"standard_rate\",\"custom_norn_ref\"]",
            filters: "[[\"custom_norn_ref\",\"is\",\"set\"],[\"modified\",\">=\",{{json:version}}]]",
            or_filters: "[[\"modified\",\">\",{{json:version}}],[\"name\",\">\",{{json:id}}]]",
            order_by: "modified asc, name asc",
            limit_start: "0",
            limit_page_length: "{{size}}"
          }
        },
        itemsPath: "/data",
        paging: {kind: "keyset", size: 100},
        idPath: "/name",
        versionPath: "/modified",
        fields: {
          name: "/name",
          custom_norn_ref: "/custom_norn_ref",
          code: "/item_code",
          label: "/item_name",
          sku: "/custom_sku",
          price: "/standard_rate"
        },
        initial: {version: "1970-01-01 00:00:00.000000", id: ""}
      },
      {
        dataset: "Warehouse Lot Balance",
        request: {
          method: "GET",
          path: "/api/resource/Warehouse%20Lot%20Balance",
          query: {
            fields: "[\"name\",\"modified\",\"custom_norn_ref\",\"resource_code\",\"quantity\",\"unit\",\"location\",\"item_ref\",\"owner_ref\",\"custodian_ref\",\"stock_cost\",\"cost_currency\",\"valuation_method\"]",
            filters: "[[\"custom_norn_ref\",\"is\",\"set\"],[\"modified\",\">=\",{{json:version}}]]",
            or_filters: "[[\"modified\",\">\",{{json:version}}],[\"name\",\">\",{{json:id}}]]",
            order_by: "modified asc, name asc",
            limit_start: "0",
            limit_page_length: "{{size}}"
          }
        },
        itemsPath: "/data",
        paging: {kind: "keyset", size: 100},
        idPath: "/name",
        versionPath: "/modified",
        fields: {
          name: "/name",
          custom_norn_ref: "/custom_norn_ref",
          resource_code: "/resource_code",
          quantity: "/quantity",
          unit: "/unit",
          location: "/location",
          item_ref: "/item_ref",
          owner_ref: "/owner_ref",
          custodian_ref: "/custodian_ref",
          stock_cost: "/stock_cost",
          cost_currency: "/cost_currency",
          valuation_method: "/valuation_method",
          modified: "/modified"
        },
        initial: {version: "1970-01-01 00:00:00.000000", id: ""}
      },
      {
        dataset: "Sales Order Item",
        request: {
          method: "GET",
          path: "/api/resource/Sales%20Order%20Item",
          query: {
            parent: "Sales Order",
            fields: "[\"name\",\"modified\",\"qty\",\"uom\",\"custom_norn_ref\",\"custom_due_at\",\"custom_provider_ref\",\"custom_receiver_ref\",\"custom_holding_ref\"]",
            filters: "[[\"custom_norn_ref\",\"is\",\"set\"],[\"modified\",\">=\",{{json:version}}]]",
            or_filters: "[[\"modified\",\">\",{{json:version}}],[\"name\",\">\",{{json:id}}]]",
            order_by: "modified asc, name asc",
            limit_start: "0",
            limit_page_length: "{{size}}"
          }
        },
        itemsPath: "/data",
        paging: {kind: "keyset", size: 100},
        idPath: "/name",
        versionPath: "/modified",
        fields: {
          name: "/name",
          custom_norn_ref: "/custom_norn_ref",
          quantity: "/qty",
          unit: "/uom",
          due_at: "/custom_due_at",
          provider_ref: "/custom_provider_ref",
          receiver_ref: "/custom_receiver_ref",
          holding_ref: "/custom_holding_ref"
        },
        initial: {version: "1970-01-01 00:00:00.000000", id: ""}
      }
    ]
  }
})
