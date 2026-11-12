import { Norn } from "@norn/authoring"

// Egyptian business Arabic for the core and canon model the three-systems demo installs: every canon
// type and the core interfaces it builds on, with every property and enum value of each, except
// ISO 4217 currency codes, which are shown as the code.
// Not labelled here: `core.Conversion`, `core.Exchange`, `core.Process`, `core.analytics.*` and
// `norn.ObservedLinkClaim`, which are outside the set this pack is admitted with. The pack declares
// no `requiredLocales`, so the coverage lint does not list them.
// A locale pack: it labels types other packs own, without touching them. Machine-proposed,
// so every Arabic label is marked for review (LABEL-LOCALE-1, norn-platform#3438).
const quantity = { ar: "الكمية" }, unit = { ar: "الوحدة" }, currency = { ar: "العملة" }, amount = { ar: "المبلغ" }
const commitmentStatus = { requested: { ar: "مطلوب" }, promised: { ar: "موعود به" }, fulfilled: { ar: "منفَّذ" }, cancelled: { ar: "ملغى" }, breached: { ar: "مُخلّ به" } }
const valuation = { fifo: { ar: "الوارد أولًا يُصرف أولًا" }, moving_average: { ar: "المتوسط المتحرك" } }
const settlementKind = { payment: { ar: "دفعة" }, refund: { ar: "استرداد" } }
const catalogProperties = { id: { ar: "المعرّف" }, code: { ar: "الكود" }, label: { ar: "الاسم" } }
const eventAction = { transfer_ownership: { ar: "نقل الملكية" }, transfer_custody: { ar: "نقل الحيازة" }, consume: { ar: "استهلاك" }, produce: { ar: "إنتاج" } }

export const ArabicLabels = Norn.labels({
  id: "locale.ar_eg.labels",
  title: "Egyptian Arabic labels for the core and canon model",
  version: "0.1.0",
  needsReview: ["ar"],
  types: {
    "core.Agent": { label: { ar: "جهة" }, properties: { display_name: { ar: "الاسم" }, kind: { ar: "النوع" } }, values: { kind: { person: { ar: "شخص" }, organization: { ar: "مؤسسة" }, system: { ar: "نظام" } } } },
    "core.AgentRole": { label: { ar: "دور الجهة" }, properties: { role: { ar: "الدور" }, valid_from: { ar: "ساري من" }, valid_to: { ar: "ساري حتى" } } },
    "core.Claim": { label: { ar: "مطالبة" }, properties: { amount, currency, due_date: { ar: "تاريخ الاستحقاق" } } },
    "core.Commitment": { label: { ar: "التزام" }, properties: { status: { ar: "الحالة" }, due_at: { ar: "موعد الاستحقاق" }, quantity, unit, currency }, values: { status: commitmentStatus } },
    "core.EconomicEvent": { label: { ar: "حدث اقتصادي" }, properties: { action: { ar: "الحركة" }, occurred_at: { ar: "وقت الحدوث" }, quantity, unit, currency }, values: { action: eventAction } },
    "core.Hold": {
      label: { ar: "حجز" },
      properties: { status: { ar: "الحالة" }, quantity, unit, currency, expires_at: { ar: "وقت الانتهاء" } },
      values: { status: { open: { ar: "مفتوح" }, released: { ar: "مُفرَج عنه" }, consumed: { ar: "مُستهلَك" }, expired: { ar: "منتهٍ" } } },
    },
    "core.LedgerEffect": { label: { ar: "أثر في الدفتر" }, properties: { account_code: { ar: "كود الحساب" }, amount, currency } },
    "core.Resource": { label: { ar: "مورد" }, properties: { resource_code: { ar: "كود المورد" }, quantity, unit, currency, valuation_method: { ar: "طريقة التقييم" } }, values: { valuation_method: valuation } },
    "canon.Catalog": { label: { ar: "كتالوج" }, localizedDescription: { ar: "بيانات وصفية للكتالوج، وليست مخزونًا" }, properties: catalogProperties },
    "canon.Settlement": { label: { ar: "تسوية" }, localizedDescription: { ar: "دليل تسوية مُبلَّغ عنه" }, properties: { amount, currency, kind: { ar: "النوع" } }, values: { kind: settlementKind } },
    "canon.Item": { label: { ar: "صنف" }, properties: catalogProperties },
    "canon.Holding": {
      label: { ar: "رصيد مخزون" }, localizedDescription: { ar: "رصيد مقسَّم حسب المالك والموقع" },
      properties: {
        resource_code: { ar: "كود المورد" }, quantity, unit, currency, valuation_method: { ar: "طريقة التقييم" },
        observed_at: { ar: "وقت الرصد" }, location: { ar: "المخزن" }, quantity_basis: { ar: "أساس الكمية" }, stock_cost: { ar: "تكلفة المخزون" }, cost_currency: { ar: "عملة التكلفة" },
      },
      values: {
        valuation_method: valuation,
        quantity_basis: { on_hand: { ar: "الرصيد الفعلي" }, available: { ar: "المتاح" } },
      },
    },
    "canon.Delivery": { label: { ar: "تسليم" }, localizedDescription: { ar: "تسليم مرصود" }, properties: { action: { ar: "الحركة" }, occurred_at: { ar: "وقت الحدوث" }, quantity, unit, currency }, values: { action: eventAction } },
    "canon.OrderLine": { label: { ar: "بند أمر بيع" }, localizedDescription: { ar: "التزام على مستوى بند الأمر" }, properties: { status: { ar: "الحالة" }, due_at: { ar: "موعد الاستحقاق" }, quantity, unit, currency }, values: { status: commitmentStatus } },
    "canon.Party": { label: { ar: "طرف" }, properties: { display_name: { ar: "الاسم" }, kind: { ar: "النوع" } }, values: { kind: { person: { ar: "شخص" }, organization: { ar: "مؤسسة" }, system: { ar: "نظام" } } } },
    "canon.PartyRole": { label: { ar: "دور الطرف" }, localizedDescription: { ar: "دور الطرف كما ورد في المصدر" }, properties: { role: { ar: "الدور" }, valid_from: { ar: "ساري من" }, valid_to: { ar: "ساري حتى" } } },
    "canon.Posting": { label: { ar: "ترحيل" }, localizedDescription: { ar: "قيد دفتري مرصود بإشارة، وليس أمر ترحيل" }, properties: { account_code: { ar: "كود الحساب" }, amount, currency, book: { ar: "الدفتر" }, account_role: { ar: "دور الحساب" } } },
    "canon.ValuationPolicy": {
      label: { ar: "سياسة تقييم المخزون" }, localizedDescription: { ar: "سياسة تقييم المخزون" },
      properties: { id: { ar: "المعرّف" }, label: { ar: "الاسم" }, valuation_method: { ar: "طريقة التقييم" } },
      values: { valuation_method: valuation },
    },
    "canon.SettlementObservation": { label: { ar: "تسوية مرصودة" }, localizedDescription: { ar: "تسوية مرصودة من المصدر" }, properties: { amount, currency, kind: { ar: "النوع" } }, values: { kind: settlementKind } },
  },
  actions: {
    "canon.valuation_policy.set": { label: { ar: "تحديد سياسة تقييم المخزون" }, localizedDescription: { ar: "إنشاء سياسة تقييم مخزون أو تحديثها." } },
  },
})
