import { Norn } from "@norn/authoring"

export const Contacts = Norn.sourceSchema({
  id: "toy.contacts.source",
  title: "Synthetic contact input",
  version: "1.0.0",
  source: { system: "Toy", instance: "independent-fixture" },
  observedAt: "2026-10-01T00:00:00Z",
  adapter: { name: "toy.fixture", version: "1.0.0" },
  rawMetadata: { uri: "evidence://independent/contact-fixture", sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
  datasets: [{ id: "Rows", nativeName: "Rows", kind: "table" }],
  fields: [{ dataset: "Rows", name: "customer", ordinal: 0, nativeType: "text", typeCandidate: { type: "string", origin: "declared" }, required: true }],
})
