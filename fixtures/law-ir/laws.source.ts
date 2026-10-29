import { Norn } from "@norn/authoring"

export const no_unpaid_ship = Norn.law({
  "id": "erpnext.no_unpaid_ship.law",
  "title": "No order ships unpaid.",
  "version": "0.1.0",
  "law": {
    "lawVersion": "0.1",
    "id": "erpnext.no_unpaid_ship",
    "owner": "erpnext.controls",
    "severity": "error",
    "rationale": "No order ships unpaid.",
    "provenance": {
      "source": "authoring_sdk",
      "sourcePath": "fixtures/law-ir/laws.source.ts"
    },
    "scope": {
      "root": {
        "kind": "app",
        "id": "erpnext"
      },
      "objectTypes": [
        "erpnext.SalesOrder"
      ]
    },
    "subject": "erpnext.SalesOrder",
    "modality": {
      "kind": "never"
    },
    "response": {
      "kind": "predicate",
      "expression": "self.status == 'shipped' and self.paid == false"
    }
  },
  "dependencies": [
    {
      "artifactId": "erpnext.SalesOrder.entity",
      "kind": "entity",
      "version": "0.1.0",
      "required": true
    }
  ]
})

export const approved_amount = Norn.law({
  "id": "erpnext.approved_amount.law",
  "title": "Approved PO amount never changes from the exact approval snapshot.",
  "version": "0.1.0",
  "law": {
    "lawVersion": "0.1",
    "id": "erpnext.approved_amount",
    "owner": "erpnext.controls",
    "severity": "error",
    "rationale": "Approved PO amount never changes from the exact approval snapshot.",
    "provenance": {
      "source": "authoring_sdk",
      "sourcePath": "fixtures/law-ir/laws.source.ts"
    },
    "scope": {
      "root": {
        "kind": "app",
        "id": "erpnext"
      },
      "objectTypes": [
        "erpnext.PurchaseOrder"
      ]
    },
    "subject": "erpnext.PurchaseOrder",
    "modality": {
      "kind": "never"
    },
    "response": {
      "kind": "predicate",
      "expression": "self.amount != self.approved_amount"
    },
    "condition": "self.status == 'approved'"
  },
  "dependencies": [
    {
      "artifactId": "erpnext.PurchaseOrder.entity",
      "kind": "entity",
      "version": "0.1.0",
      "required": true
    }
  ]
})

export const request_terminal = Norn.law({
  "id": "erpnext.request_terminal.law",
  "title": "Every request eventually reaches a terminal state within 30 days.",
  "version": "0.1.0",
  "law": {
    "lawVersion": "0.1",
    "id": "erpnext.request_terminal",
    "owner": "erpnext.controls",
    "severity": "error",
    "rationale": "Every request eventually reaches a terminal state within 30 days.",
    "provenance": {
      "source": "authoring_sdk",
      "sourcePath": "fixtures/law-ir/laws.source.ts"
    },
    "scope": {
      "root": {
        "kind": "app",
        "id": "erpnext"
      },
      "objectTypes": [
        "erpnext.Request"
      ],
      "commitments": [
        "erpnext.Request"
      ]
    },
    "subject": "erpnext.Request",
    "modality": {
      "kind": "eventually"
    },
    "response": {
      "kind": "predicate",
      "expression": "self.status == 'accepted' or self.status == 'rejected'"
    },
    "timing": {
      "kind": "within",
      "millis": "2592000000"
    }
  },
  "dependencies": [
    {
      "artifactId": "erpnext.Request.entity",
      "kind": "entity",
      "version": "0.1.0",
      "required": true
    }
  ]
})

export const open_until_paid = Norn.law({
  "id": "erpnext.open_until_paid.law",
  "title": "An order stays open until paid (strong until).",
  "version": "0.1.0",
  "law": {
    "lawVersion": "0.1",
    "id": "erpnext.open_until_paid",
    "owner": "erpnext.controls",
    "severity": "error",
    "rationale": "An order stays open until paid (strong until).",
    "provenance": {
      "source": "authoring_sdk",
      "sourcePath": "fixtures/law-ir/laws.source.ts"
    },
    "scope": {
      "root": {
        "kind": "app",
        "id": "erpnext"
      },
      "objectTypes": [
        "erpnext.SalesOrder"
      ]
    },
    "subject": "erpnext.SalesOrder",
    "modality": {
      "kind": "until",
      "predicate": "self.paid == true"
    },
    "response": {
      "kind": "predicate",
      "expression": "self.status == 'open'"
    }
  },
  "dependencies": [
    {
      "artifactId": "erpnext.SalesOrder.entity",
      "kind": "entity",
      "version": "0.1.0",
      "required": true
    }
  ]
})

export const invariant_identity = Norn.law({
  "id": "erpnext.invariant_identity.law",
  "title": "Reuse the PO invariant by identity.",
  "version": "0.1.0",
  "law": {
    "lawVersion": "0.1",
    "id": "erpnext.invariant_identity",
    "owner": "erpnext.controls",
    "severity": "error",
    "rationale": "Reuse the PO invariant by identity.",
    "provenance": {
      "source": "authoring_sdk",
      "sourcePath": "fixtures/law-ir/laws.source.ts"
    },
    "scope": {
      "root": {
        "kind": "app",
        "id": "erpnext"
      },
      "objectTypes": [
        "erpnext.PurchaseOrder"
      ]
    },
    "subject": "erpnext.PurchaseOrder",
    "modality": {
      "kind": "always"
    },
    "response": {
      "kind": "invariant",
      "objectType": "erpnext.PurchaseOrder",
      "invariantId": "amount_frozen"
    }
  },
  "dependencies": [
    {
      "artifactId": "erpnext.PurchaseOrder.entity",
      "kind": "entity",
      "version": "0.1.0",
      "required": true
    }
  ]
})

