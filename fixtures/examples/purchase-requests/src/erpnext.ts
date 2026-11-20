// ERPNext as the source of suppliers and existing material requests: the
// source schema as ERPNext describes the two doctypes, and the mapping onto
// the purchasing types. The mapping binds the schema by reference, so the
// checker pins its hash; nothing is pasted.
import { Norn } from "@norn/authoring"
import { Priced, PurchaseRequest, Supplier } from "./purchasing.ts"

const CURRENCIES = [{ value: "EGP" }, { value: "USD" }]

export const ErpnextSchema = Norn.sourceSchema({
  id: "purchasing.erpnext.source", title: "ERPNext suppliers and material requests", version: "0.1.0",
  source: { system: "erpnext", instance: "demo", version: "v15" },
  observedAt: "2026-11-19T00:00:00Z",
  adapter: { name: "recorded-export", version: "0.1.0" },
  rawMetadata: { uri: "fixtures/examples/purchase-requests/data/erpnext.json", sha256: "8ab6c31ea13fcc5955ba8f05f0570e67638260abede6b4a84495a4eff0c2bc36" },
  datasets: [
    { id: "Supplier", nativeName: "tabSupplier", kind: "document" },
    { id: "Material Request", nativeName: "tabMaterial Request", kind: "document" },
  ],
  fields: [
    { dataset: "Supplier", name: "name", ordinal: 0, nativeType: "Data", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Supplier", name: "supplier_name", ordinal: 1, nativeType: "Data", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Supplier", name: "default_currency", ordinal: 2, nativeType: "Link", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Material Request", name: "name", ordinal: 0, nativeType: "Data", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Material Request", name: "owner", ordinal: 1, nativeType: "Link", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Material Request", name: "supplier", ordinal: 2, nativeType: "Link", typeCandidate: { type: "string", origin: "declared" }, required: true },
    { dataset: "Material Request", name: "currency", ordinal: 3, nativeType: "Link", typeCandidate: { type: "string", origin: "declared" }, required: true },
    // ERPNext sends currency amounts as exact decimal text.
    { dataset: "Material Request", name: "grand_total", ordinal: 4, nativeType: "Currency", typeCandidate: { type: "decimal", origin: "declared" }, required: true },
    { dataset: "Material Request", name: "workflow_state", ordinal: 5, nativeType: "Select", typeCandidate: { type: "string", origin: "declared" }, required: true },
  ],
  codeSets: [
    { id: "supplier_currency", dataset: "Supplier", field: "default_currency", origin: "declared", values: CURRENCIES },
    { id: "request_currency", dataset: "Material Request", field: "currency", origin: "declared", values: CURRENCIES },
    { id: "request_state", dataset: "Material Request", field: "workflow_state", origin: "declared", values: [{ value: "Draft" }, { value: "Pending" }, { value: "Approved" }, { value: "Rejected" }] },
  ],
  constraints: [
    { id: "supplier_name_key", kind: "primary_key", dataset: "Supplier", fields: ["name"], origin: "declared" },
    { id: "request_name_key", kind: "primary_key", dataset: "Material Request", fields: ["name"], origin: "declared" },
    { id: "request_supplier", kind: "foreign_key", dataset: "Material Request", fields: ["supplier"], target: { dataset: "Supplier", fields: ["name"] }, origin: "declared" },
  ],
})

export const ErpnextMapping = Norn.mapping({
  id: "purchasing.erpnext.mapping", title: "ERPNext to purchasing", version: "0.1.0",
  source: ErpnextSchema,
  sourceContract: { kind: "generic" },
  interfaces: [Priced],
  maps: [
    {
      id: "supplier",
      source: { dataset: "Supplier" },
      target: { type: Supplier, interface: Priced },
      subject: "input.name",
      sourceRecord: "'erpnext/demo/Supplier/' + input.name",
      properties: [
        { property: "supplier_id", value: "input.name" },
        { property: "supplier_name", value: "input.supplier_name" },
        { property: "currency", value: "input.default_currency" },
      ],
    },
    {
      id: "request",
      source: { dataset: "Material Request" },
      target: { type: PurchaseRequest, interface: Priced },
      subject: "input.name",
      sourceRecord: "'erpnext/demo/Material Request/' + input.name",
      properties: [
        { property: "request_id", value: "input.name" },
        { property: "amount", value: "input.grand_total" },
        { property: "currency", value: "input.currency" },
        { property: "requester", value: "input.owner" },
        { property: "supplier_id", value: "input.supplier" },
      ],
      links: [{ link: "supplier", targetMap: "supplier", key: "input.supplier" }],
      lifecycles: [{
        property: "status",
        state: "input.workflow_state == 'Approved' ? 'approved' : input.workflow_state == 'Rejected' ? 'rejected' : input.workflow_state == 'Pending' ? 'submitted' : 'draft'",
      }],
    },
  ],
})
