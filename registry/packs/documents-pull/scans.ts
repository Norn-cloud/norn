import { Norn } from "@norn/authoring"

// The scanned-document extractor as a pull source (#921): the cell reads the
// extractor's rows through the generic pull engine (`norn.pull/1`) and maps
// them with the documents pack's own `docs.scans.mapping`, so a scan lands
// exactly as a recorded snapshot does. The extractor answers
// `POST /extractions {after: [version, id], limit}` with `{rows: [...]}` in
// (version, id) order; each row is one Scanned Document with its printed
// lines and field evidence nested. The connection (base URL) is server
// configuration on the private network; the extractor takes no credential.

export const ScansConnector = Norn.connector({
  id: "docs.scans.connector",
  title: "Scanned-document extractor reads",
  version: "0.1.0",
  auth: {kind: "none"},
  resources: [
    {
      kind: "delta",
      resourceId: "extractions",
      endpoint: "/extractions",
      sinceParam: "after",
      updatedAtPath: "/version",
      itemsPath: "/rows"
    }
  ],
  mappings: [],
  pull: {
    source: "synthetic-scans",
    pipeline: "norn-pull/1+docs.scans.connector@0.1.0",
    feeds: [
      {
        dataset: "Scanned Document",
        request: {
          method: "POST",
          path: "/extractions",
          body: {after: ["{{version}}", "{{id}}"], limit: "{{size}}"}
        },
        itemsPath: "/rows",
        paging: {kind: "keyset", size: 100},
        idPath: "/id",
        versionPath: "/version",
        fields: {
          name: "/name",
          doc_number: "/doc_number",
          kind: "/kind",
          vendor: "/vendor",
          issued_on: "/issued_on",
          currency: "/currency",
          total: "/total",
          line_count: "/line_count",
          ledger_ref: "/ledger_ref",
          document_uri: "/document_uri",
          document_digest: "/document_digest",
          media_type: "/media_type",
          extractor: "/extractor",
          extractor_version: "/extractor_version",
          lines: "/lines",
          evidence: "/evidence"
        },
        initial: {version: "", id: ""}
      }
    ]
  }
})
