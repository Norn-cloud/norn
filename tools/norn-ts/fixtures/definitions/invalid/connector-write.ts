import { Norn } from "@norn/authoring"
export const Invalid = Norn.connector({
  id: "private.invalid", title: "Invalid", version: "0.1.0", auth: { kind: "none" },
  resources: [{ kind: "paginated", resourceId: "records", endpoint: "/records", method: "DELETE", pageParam: "page", itemsPath: "/items" }], mappings: [],
})
