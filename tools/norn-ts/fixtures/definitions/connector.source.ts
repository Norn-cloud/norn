import { Norn } from "@norn/authoring"

export const Mail = Norn.connector({
  id: "private.mail", title: "Mailbox", version: "0.1.0",
  auth: { kind: "oauth2", authId: "mail", authorizationUrl: "https://example.com/oauth/authorize", tokenUrl: "https://example.com/oauth/token", scopes: ["mail.readonly"], clientSecretRef: "MAIL_CLIENT_SECRET" },
  resources: [{ kind: "cursor", resourceId: "messages", endpoint: "/messages", cursorParam: "pageToken", nextCursorPath: "/nextPageToken", itemsPath: "/messages", missingItemsIsEmpty: true }],
  mappings: [{ kind: "external_id", resourceId: "messages", sourcePath: "/id", targetObjectKind: "mail.Message", externalSource: "mail" }],
  cursor: { cursorId: "mail", resourceId: "messages", storageKey: "mail.messages" },
  deadLetterPolicy: { maxFailures: 3, route: "ledger" },
  poll: { feeds: [{ resourceId: "messages", recordKeyPath: "/id" }] },
})

export const Ledger = Norn.connector({
  id: "private.ledger", title: "Ledger", version: "0.1.0",
  auth: { kind: "api_key", authId: "ledger", placement: "header", name: "Authorization", secretRef: "LEDGER_READ_KEY", prefix: "token ", format: { kind: "pair", separator: ":" } },
  resources: [{ kind: "paginated", resourceId: "entries", endpoint: "/entries", method: "GET", pageParam: "start", pageSizeParam: "limit", pageMode: "offset", pageStart: 0, pageSize: 100, itemsPath: "/data", queryParams: { fields: "id" } }],
  mappings: [{ kind: "field", resourceId: "entries", sourcePath: "/id", targetObjectKind: "ledger.Entry", targetField: "name" }],
})

export const Public = Norn.connector({
  id: "private.public", title: "Public", version: "0.1.0", auth: { kind: "none" },
  resources: [{ kind: "delta", resourceId: "events", endpoint: "/events", sinceParam: "since", updatedAtPath: "/updated", itemsPath: "/events" }],
  mappings: [],
})
