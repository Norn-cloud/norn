export type RendererLocale = "en" | "ar"
export const en = {
  "analysis.predictions": "Held-out predictions",
  "analysis.predictionAxes": "x: predicted outcome · y: observed outcome",
  "analysis.plotUnavailable": "{{title}}: plot data unavailable within the display bounds.",
  "analysis.plotData": "Inspect the original plot values",
  "analysis.serverState": "Cell result state",
  "analysis.obligations": "Assumption obligations",
  "analysis.recordedOutcome": "Recorded check: {{value}}",
  "analysis.analyst": "Signed by {{analyst}}",
  "analysis.diagnostics": "Computed diagnostics",
  "analysis.check": "Declared check",
  "analysis.serverEvidence": "Recorded evidence",
  "analysis.residuals": "Residual plot",
  "analysis.residualAxes": "x: fitted value · y: residual",
  "analysis.qq": "Normal Q–Q plot",
  "analysis.qqAxes": "x: normal quantile · y: standardized residual",
  "analysis.influence": "Cook’s distance",
  "analysis.influenceAxes": "x: observation row · y: Cook’s distance",
  "analysis.presentationOnly": "Server results are displayed unchanged. Plots only scale the recorded points; checks do not prove assumptions.",
  "analysis.fullReport": "Inspect the complete server report",
}
export const ar = {
  "analysis.predictions": "التنبؤات على العينة المحجوزة",
  "analysis.predictionAxes": "س: النتيجة المتنبأ بها · ص: النتيجة المرصودة",
  "analysis.plotUnavailable": "{{title}}: بيانات الرسم غير متاحة ضمن حدود العرض.",
  "analysis.plotData": "فحص قيم الرسم الأصلية",
  "analysis.serverState": "حالة النتيجة من الخلية",
  "analysis.obligations": "التزامات الافتراضات",
  "analysis.recordedOutcome": "الفحص المسجل: {{value}}",
  "analysis.analyst": "وقّعها {{analyst}}",
  "analysis.diagnostics": "الفحوص المحسوبة",
  "analysis.check": "الفحص المعلن",
  "analysis.serverEvidence": "الأدلة المسجلة",
  "analysis.residuals": "رسم البواقي",
  "analysis.residualAxes": "س: القيمة المقدرة · ص: الباقي",
  "analysis.qq": "رسم الكميات الطبيعية Q–Q",
  "analysis.qqAxes": "س: الكمية الطبيعية · ص: الباقي المعياري",
  "analysis.influence": "مسافة كوك",
  "analysis.influenceAxes": "س: صف الملاحظة · ص: مسافة كوك",
  "analysis.presentationOnly": "تُعرض نتائج الخادم دون تغيير. يغيّر الرسم مقياس النقاط المسجلة فقط؛ الفحوص لا تثبت الافتراضات.",
  "analysis.fullReport": "فحص تقرير الخادم الكامل",
}
export type RendererMessageKey = keyof typeof en
export function message(locale: string, key: RendererMessageKey, vars: Readonly<Record<string, string | number | undefined>> = {}): string {
  const language: RendererLocale = locale.toLowerCase().startsWith("ar") ? "ar" : "en"
  const catalog: Readonly<Record<string, string>> = language === "ar" ? ar : en
  const count = vars.count
  const form = typeof count === "number" ? new Intl.PluralRules(language).select(count) : undefined
  const template = (form ? catalog[`${key}_${form}`] ?? catalog[`${key}_other`] : undefined) ?? catalog[key] ?? key
  return template.replace(/{{(\w+)}}/g, (placeholder, name: string) => vars[name] === undefined ? placeholder : String(vars[name]))
}
