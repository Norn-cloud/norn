import { Norn } from "@norn/authoring"
const auth = { kind: "api_key", authId: "test", placement: "header", name: "Authorization", secretRef: "READ_KEY", secret: "literal-credential" } as const
export const Invalid = Norn.connector({
  id: "private.invalid", title: "Invalid", version: "0.1.0", auth,
  resources: [{ kind: "cursor", resourceId: "records", endpoint: "/records", cursorParam: "cursor", nextCursorPath: "/next", itemsPath: "/items" }], mappings: [],
})
