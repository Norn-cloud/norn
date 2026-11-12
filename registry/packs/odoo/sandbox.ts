import { Norn } from "@norn/authoring"
import { Agent, AgentRole, Resource, Commitment } from "../../core/src/physics.ts"
import { Catalog, Item, Party, PartyRole, Holding, OrderLine } from "../canon/records.ts"

// A live Odoo sandbox (#921): the same rows as the synthetic export, read
// through Odoo's JSON-2 API (`web_search_read` with a field specification, so
// owner, location, product and customer come through Odoo's own relations).
// The x_ fields are the canonical references demo/sandboxes/odoo/seed.py
// creates. Feeds read rows after (write_date, id) in that order, only rows the
// organisation has referenced; a lone `{{id}}` keeps the integer type. The
// connection (base URL and API key) is server configuration.

export const SandboxSource = Norn.sourceSchema({
  id: "odoo.sandbox.source",
  title: "Odoo sandbox: partners, products, quants and sale order lines",
  version: "0.1.0",
  source: {system:  "odoo", instance:  "sandbox", version:  "odoo-19"},
  observedAt: "2026-11-12T00:00:00Z",
  adapter: {name:  "norn-pull", version:  "1"},
  rawMetadata: {uri:  "demo/sandboxes/odoo/seed.py", sha256:  "5bd37affe2edd17f4dad3c948f10d898a46bf92088adb5b9b9fb6d8c348978ed"},
  datasets: [
    {id:  "product.product", nativeName:  "product.product", kind:  "document"},
    {id:  "stock.quant", nativeName:  "stock.quant", kind:  "document"},
    {id:  "res.partner", nativeName:  "res.partner", kind:  "document"},
    {id:  "sale.order.line", nativeName:  "sale.order.line", kind:  "document"}
  ],
  fields: [
    {
      dataset: "product.product",
      name: "id",
      ordinal: 0,
      nativeType: "integer",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "product.product",
      name: "x_norn_ref",
      ordinal: 1,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "product.product",
      name: "code",
      ordinal: 2,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "product.product",
      name: "label",
      ordinal: 3,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "product.product",
      name: "sku",
      ordinal: 4,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "product.product",
      name: "price",
      ordinal: 5,
      nativeType: "float",
      typeCandidate: {type:  "decimal", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "id",
      ordinal: 0,
      nativeType: "integer",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "x_norn_ref",
      ordinal: 1,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "resource_code",
      ordinal: 2,
      nativeType: "many2one",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "quantity",
      ordinal: 3,
      nativeType: "float",
      typeCandidate: {type:  "decimal", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "unit",
      ordinal: 4,
      nativeType: "many2one",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "location",
      ordinal: 5,
      nativeType: "many2one",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "item_ref",
      ordinal: 6,
      nativeType: "many2one",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "owner_ref",
      ordinal: 7,
      nativeType: "many2one",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "custodian_ref",
      ordinal: 8,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "stock_cost",
      ordinal: 9,
      nativeType: "float",
      typeCandidate: {type:  "decimal", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "cost_currency",
      ordinal: 10,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "stock.quant",
      name: "valuation_method",
      ordinal: 11,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "res.partner",
      name: "id",
      ordinal: 0,
      nativeType: "integer",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "res.partner",
      name: "x_norn_ref",
      ordinal: 1,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "res.partner",
      name: "label",
      ordinal: 2,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "res.partner",
      name: "role",
      ordinal: 3,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "sale.order.line",
      name: "id",
      ordinal: 0,
      nativeType: "integer",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "sale.order.line",
      name: "x_norn_ref",
      ordinal: 1,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "sale.order.line",
      name: "quantity",
      ordinal: 2,
      nativeType: "float",
      typeCandidate: {type:  "decimal", origin:  "declared"},
      required: true
    },
    {
      dataset: "sale.order.line",
      name: "unit",
      ordinal: 3,
      nativeType: "many2one",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "sale.order.line",
      name: "due_at",
      ordinal: 4,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "sale.order.line",
      name: "provider_ref",
      ordinal: 5,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "sale.order.line",
      name: "receiver_ref",
      ordinal: 6,
      nativeType: "many2one",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    },
    {
      dataset: "sale.order.line",
      name: "holding_ref",
      ordinal: 7,
      nativeType: "char",
      typeCandidate: {type:  "string", origin:  "declared"},
      required: true
    }
  ],
  codeSets: [
    {id:  "holding_unit", dataset:  "stock.quant", field:  "unit", origin:  "declared", values:  [{value:  "g"}]},
    {id:  "order_unit", dataset:  "sale.order.line", field:  "unit", origin:  "declared", values:  [{value:  "g"}]},
    {
      id: "holding_valuation_method",
      dataset: "stock.quant",
      field: "valuation_method",
      origin: "declared",
      values: [{value:  "moving_average"}]
    }
  ],
  constraints: [
    {id:  "source_id_0", kind:  "primary_key", dataset:  "product.product", fields:  ["id"], origin:  "declared"},
    {id:  "source_id_1", kind:  "primary_key", dataset:  "stock.quant", fields:  ["id"], origin:  "declared"},
    {id:  "source_id_2", kind:  "primary_key", dataset:  "res.partner", fields:  ["id"], origin:  "declared"},
    {id:  "source_id_3", kind:  "primary_key", dataset:  "sale.order.line", fields:  ["id"], origin:  "declared"}
  ]
})

export const SandboxMapping = Norn.mapping({
  id: "odoo.sandbox.mapping",
  title: "Odoo sandbox observations to the REA canon",
  version: "0.1.0",
  source: {schemaId:  "odoo.sandbox.source", version:  "0.1.0", sha256:  "0000000000000000000000000000000000000000000000000000000000000000"},
  sourceContract: {kind:  "generic"},
  interfaces: [Catalog, Resource, Agent, AgentRole, Commitment],
  maps: [
    {
      id: "item",
      source: {dataset:  "product.product"},
      target: {type:  Item, interface:  Catalog},
      subject: "input.x_norn_ref",
      sourceRecord: "'odoo/sandbox/product.product/' + input.id",
      properties: [{property:  "id", value:  "input.x_norn_ref"}, {property:  "code", value:  "input.code"}, {property:  "label", value:  "input.label"}],
      ignoredFields: [
        {field:  "sku", reason:  "Alias only; duplicate SKUs do not establish identity."},
        {field:  "price", reason:  "Sales price is not stock cost, valuation or a ledger effect."}
      ]
    },
    {
      id: "holding",
      source: {dataset:  "stock.quant"},
      target: {type:  Holding, interface:  Resource},
      subject: "input.x_norn_ref",
      sourceRecord: "'odoo/sandbox/stock.quant/' + input.id",
      properties: [
        {property:  "resource_code", value:  "input.resource_code"},
        {property:  "quantity", value:  "input.quantity"},
        {property:  "unit", value:  "input.unit"},
        {property:  "currency", value:  "null"},
        {property:  "valuation_method", value:  "input.valuation_method"},
        {property:  "location", value:  "input.location"},
        {property:  "quantity_basis", value:  "'on_hand'"},
        {property:  "stock_cost", value:  "input.stock_cost"},
        {property:  "cost_currency", value:  "input.cost_currency"}
      ],
      links: [
        {link:  "item", targetMap:  "item", key:  "input.item_ref"},
        {link:  "owner", targetMap:  "party", key:  "input.owner_ref"},
        {link:  "custodian", targetMap:  "party", key:  "input.custodian_ref"}
      ]
    },
    {
      id: "party",
      source: {dataset:  "res.partner"},
      target: {type:  Party, interface:  Agent},
      subject: "input.x_norn_ref",
      sourceRecord: "'odoo/sandbox/res.partner/' + input.id",
      properties: [{property:  "display_name", value:  "input.label"}, {property:  "kind", value:  "'organization'"}],
      ignoredFields: [{field:  "role", reason:  "Mapped separately as an AgentRole; a customer role is not an Agent subtype."}]
    },
    {
      id: "party_role",
      source: {dataset:  "res.partner"},
      target: {type:  PartyRole, interface:  AgentRole},
      subject: "input.x_norn_ref + '/role/' + input.role",
      sourceRecord: "'odoo/sandbox/res.partner/' + input.id",
      properties: [{property:  "role", value:  "input.role"}, {property:  "valid_from", value:  "null"}, {property:  "valid_to", value:  "null"}],
      links: [{link:  "agent", targetMap:  "party", key:  "input.x_norn_ref"}],
      gaps: [
        {
          member: "counterparty",
          reason: "not_in_source",
          note: "The role is scoped to the demo organisation, not an inferred bilateral legal relationship."
        }
      ],
      ignoredFields: [{field:  "label", reason:  "Agent label is mapped by party."}]
    },
    {
      id: "order",
      source: {dataset:  "sale.order.line"},
      target: {type:  OrderLine, interface:  Commitment},
      subject: "input.x_norn_ref",
      sourceRecord: "'odoo/sandbox/sale.order.line/' + input.id",
      properties: [
        {property:  "quantity", value:  "input.quantity"},
        {property:  "unit", value:  "input.unit"},
        {property:  "currency", value:  "null"},
        {property:  "status", value:  "'promised'"},
        {property:  "due_at", value:  "timestamp(input.due_at)"}
      ],
      links: [
        {link:  "provider", targetMap:  "party", key:  "input.provider_ref"},
        {link:  "receiver", targetMap:  "party", key:  "input.receiver_ref"},
        {link:  "resource", targetMap:  "holding", key:  "input.holding_ref"}
      ]
    }
  ]
})

export const SandboxConnector = Norn.connector({
  id: "odoo.sandbox.connector",
  title: "Odoo sandbox JSON-2 reads",
  version: "0.1.0",
  auth: {
    kind: "api_key",
    authId: "odoo_sandbox",
    placement: "header",
    name: "Authorization",
    secretRef: "odoo_sandbox_api_key",
    prefix: "bearer "
  },
  resources: [
    {
      kind: "paginated",
      resourceId: "web_search_read",
      endpoint: "/json/2/{model}/web_search_read",
      method: "POST",
      pageParam: "offset",
      itemsPath: "/records"
    }
  ],
  mappings: [],
  pull: {
    source: "odoo",
    pipeline: "norn-pull/1+odoo.sandbox.connector@0.1.0",
    feeds: [
      {
        dataset: "res.partner",
        request: {
          method: "POST",
          path: "/json/2/res.partner/web_search_read",
          headers: {"Content-Type": "application/json"},
          body: {
            domain: [["x_norn_ref", "!=", false], "|", ["write_date", ">", "{{version}}"], "&", ["write_date", "=", "{{version}}"], ["id", ">", "{{id}}"]],
            specification: {write_date:  {}, x_norn_ref:  {}, name:  {}, x_norn_role:  {}},
            order: "write_date asc, id asc",
            limit: "{{size}}"
          }
        },
        itemsPath: "/records",
        paging: {kind:  "keyset", size:  100},
        idPath: "/id",
        versionPath: "/write_date",
        fields: {id:  "text:/id", x_norn_ref:  "/x_norn_ref", label:  "/name", role:  "/x_norn_role"},
        initial: {version:  "1970-01-01 00:00:00", id:  0}
      },
      {
        dataset: "product.product",
        request: {
          method: "POST",
          path: "/json/2/product.product/web_search_read",
          headers: {"Content-Type": "application/json"},
          body: {
            domain: [["x_norn_ref", "!=", false], "|", ["write_date", ">", "{{version}}"], "&", ["write_date", "=", "{{version}}"], ["id", ">", "{{id}}"]],
            specification: {write_date:  {}, x_norn_ref:  {}, default_code:  {}, name:  {}, barcode:  {}, list_price:  {}},
            order: "write_date asc, id asc",
            limit: "{{size}}"
          }
        },
        itemsPath: "/records",
        paging: {kind:  "keyset", size:  100},
        idPath: "/id",
        versionPath: "/write_date",
        fields: {id:  "text:/id", x_norn_ref:  "/x_norn_ref", code:  "/default_code", label:  "/name", sku:  "/barcode", price:  "/list_price"},
        initial: {version:  "1970-01-01 00:00:00", id:  0}
      },
      {
        dataset: "stock.quant",
        request: {
          method: "POST",
          path: "/json/2/stock.quant/web_search_read",
          headers: {"Content-Type": "application/json"},
          body: {
            domain: [["x_norn_ref", "!=", false], "|", ["write_date", ">", "{{version}}"], "&", ["write_date", "=", "{{version}}"], ["id", ">", "{{id}}"]],
            specification: {
              write_date: {},
              x_norn_ref: {},
              quantity: {},
              x_custodian_ref: {},
              x_stock_cost: {},
              x_cost_currency: {},
              x_valuation_method: {},
              product_id: {fields:  {x_norn_ref:  {}, default_code:  {}}},
              product_uom_id: {fields:  {name:  {}}},
              location_id: {fields:  {name:  {}}},
              owner_id: {fields:  {x_norn_ref:  {}}}
            },
            order: "write_date asc, id asc",
            limit: "{{size}}"
          }
        },
        itemsPath: "/records",
        paging: {kind:  "keyset", size:  100},
        idPath: "/id",
        versionPath: "/write_date",
        fields: {
          id: "text:/id",
          x_norn_ref: "/x_norn_ref",
          resource_code: "/product_id/default_code",
          quantity: "/quantity",
          unit: "/product_uom_id/name",
          location: "/location_id/name",
          item_ref: "/product_id/x_norn_ref",
          owner_ref: "/owner_id/x_norn_ref",
          custodian_ref: "/x_custodian_ref",
          stock_cost: "/x_stock_cost",
          cost_currency: "/x_cost_currency",
          valuation_method: "/x_valuation_method"
        },
        initial: {version:  "1970-01-01 00:00:00", id:  0}
      },
      {
        dataset: "sale.order.line",
        request: {
          method: "POST",
          path: "/json/2/sale.order.line/web_search_read",
          headers: {"Content-Type": "application/json"},
          body: {
            domain: [["x_norn_ref", "!=", false], "|", ["write_date", ">", "{{version}}"], "&", ["write_date", "=", "{{version}}"], ["id", ">", "{{id}}"]],
            specification: {
              write_date: {},
              x_norn_ref: {},
              product_uom_qty: {},
              product_uom_id: {fields:  {name:  {}}},
              order_partner_id: {fields:  {x_norn_ref:  {}}},
              x_provider_ref: {},
              x_holding_ref: {},
              x_due_at: {}
            },
            order: "write_date asc, id asc",
            limit: "{{size}}"
          }
        },
        itemsPath: "/records",
        paging: {kind:  "keyset", size:  100},
        idPath: "/id",
        versionPath: "/write_date",
        fields: {
          id: "text:/id",
          x_norn_ref: "/x_norn_ref",
          quantity: "/product_uom_qty",
          unit: "/product_uom_id/name",
          due_at: "/x_due_at",
          provider_ref: "/x_provider_ref",
          receiver_ref: "/order_partner_id/x_norn_ref",
          holding_ref: "/x_holding_ref"
        },
        initial: {version:  "1970-01-01 00:00:00", id:  0}
      }
    ]
  }
})
