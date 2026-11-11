import { Norn } from "@norn/authoring"
import { Agent, Claim } from "../../core/src/physics.ts"
import { Invoice, Party } from "../canon/records.ts"

// UBL 2.3 Invoice → the REA canon. `Source` is generated from the hash-pinned
// OASIS originals by `norn-format-read` (see source.read.json); this file is
// the only hand-written mapping. Executable invoice rules (sum of lines, VAT
// breakdown) belong to the EN 16931 pack, not here.
export const Mapping = Norn.mapping({
  id: "ubl.invoice.mapping",
  title: "UBL 2.3 Invoice to the REA canon",
  version: "0.1.0",
  source: {schemaId: "ubl.invoice.source", version: "0.1.0", sha256: "8750751d5db261525a358ae714a568071d6e414b3cea32520ebdfc9e52be6a5a"},
  sourceContract: {kind: "generic"},
  interfaces: [Agent, Claim],
  maps: [
    {
      id: "invoice",
      source: {dataset: "InvoiceType"},
      target: {type: Invoice, interface: Claim},
      subject: "input.ID",
      sourceRecord: "'ubl/Invoice/' + input.ID",
      properties: [
        {property: "document_id", value: "input.ID"},
        {property: "issue_date", value: "input.IssueDate"},
      ],
      // UBL makes both party endpoints optional; an invoice without one is
      // refused as an invalid key, never matched to a guessed party.
      links: [{link: "creditor", targetMap: "supplier", key: "input.supplier_endpoint ?? ''"}, {link: "debtor", targetMap: "customer", key: "input.customer_endpoint ?? ''"}],
      gaps: [
        {member: "amount", reason: "deferred", note: "PayableAmount is an unbounded XSD decimal; mapping it to a money amount needs the declared scale and rounding policy of the EN 16931 pack."},
        {member: "currency", reason: "deferred", note: "DocumentCurrencyCode is optional in UBL; EN 16931 (BR-05) makes it mandatory, so the EN 16931 pack maps it."},
        {member: "due_date", reason: "deferred", note: "DueDate is optional in UBL; EN 16931 (BR-CO-25) requires a due date or payment terms, so the EN 16931 pack maps it."},
        {member: "arises_from", reason: "not_in_source", note: "OrderReference names an order by id only; the commitment it arises from needs the order system's identity."},
      ],
      ignoredFields: [
        {field: "payable_amount", reason: "Held for the amount mapping the EN 16931 pack declares."},
        {field: "DocumentCurrencyCode", reason: "Held for the currency mapping the EN 16931 pack declares."},
        {field: "DueDate", reason: "Held for the due-date mapping the EN 16931 pack declares."},
      ],
    },
    {
      id: "supplier",
      source: {dataset: "InvoiceType"},
      target: {type: Party, interface: Agent},
      subject: "input.supplier_endpoint ?? ''",
      sourceRecord: "'ubl/Invoice/' + input.ID + '/supplier'",
      properties: [{property: "display_name", value: "input.supplier_name ?? (input.supplier_endpoint ?? '')"}, {property: "kind", value: "'organization'"}],
    },
    {
      id: "customer",
      source: {dataset: "InvoiceType"},
      target: {type: Party, interface: Agent},
      subject: "input.customer_endpoint ?? ''",
      sourceRecord: "'ubl/Invoice/' + input.ID + '/customer'",
      properties: [{property: "display_name", value: "input.customer_name ?? (input.customer_endpoint ?? '')"}, {property: "kind", value: "'organization'"}],
    },
  ],
})
