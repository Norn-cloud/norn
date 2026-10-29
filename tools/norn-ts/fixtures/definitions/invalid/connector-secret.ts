import { Norn } from "@norn/authoring"
export const Invalid = Norn.connector({
  id: "private.invalid", title: "Invalid", version: "0.1.0",
  auth: { kind: "api_key", authId: "test", placement: "header", name: "Authorization", secret: "literal-credential" },
  resources: [], mappings: [],
})
