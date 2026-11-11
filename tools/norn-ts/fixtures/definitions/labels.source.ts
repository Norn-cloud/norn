import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Party = Norn.interfaceType({
  name: "sales.Party", title: "Party", version: "0.1.0",
  label: { en: "Party", ar: "طرف" },
  properties: Schema.Struct({ id: Schema.String }),
  propertyLabels: { id: { en: "Code", ar: "الكود" } },
})
export const Invoice = Norn.type({
  name: "sales.Invoice", title: "Invoice", version: "0.1.0",
  description: "A sales invoice issued to a customer.",
  label: { en: "Invoice", ar: "فاتورة", "ar-EG": "فاتورة بيع" },
  localizedDescription: { en: "A sales invoice issued to a customer.", ar: "فاتورة بيع صادرة لعميل." },
  needsReview: ["ar-EG"],
  properties: Schema.Struct({ id: Schema.String, status: Schema.Literals(["draft", "posted"]) }),
  propertyLabels: { id: { en: "Invoice number", ar: "رقم الفاتورة" }, status: { en: "Status", ar: "الحالة" } },
  valueLabels: { status: { draft: { en: "Draft", ar: "مسودة" }, posted: { en: "Posted", ar: "مُرحّلة" } } },
  identity: [{ name: "primary", properties: ["id"] }],
})
