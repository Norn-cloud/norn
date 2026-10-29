import { Norn } from "@norn/authoring"

export const law = Norn.law({
  "id": "erpnext.no_unpaid_ship.law",
  "title": "No order ships unpaid.",
  "version": "0.1.0",
  "law": {
    "id": "erpnext.no_unpaid_ship",
    "lawVersion": "0.1",
    "modality": {
      "kind": "never"
    },
    "owner": "erpnext.controls",
    "provenance": {
      "source": "authoring_sdk",
      "sourcePath": "fixtures/law-ir/laws.source.ts"
    },
    "rationale": "No order ships unpaid.",
    "response": {
      "expression": "self.status == 'shipped' and self.paid == false",
      "kind": "predicate"
    },
    "scope": {
      "objectTypes": [
        "erpnext.SalesOrder"
      ],
      "root": {
        "id": "erpnext",
        "kind": "app"
      }
    },
    "severity": "error",
    "subject": "erpnext.SalesOrder"
  },
  "dependencies": [
    {
      "artifactId": "erpnext.SalesOrder.entity",
      "kind": "entity",
      "required": true,
      "version": "0.1.0"
    }
  ]
})
