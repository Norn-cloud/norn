/**
 * Egypt localization pack (`loc-eg`): Egyptian Arabic labels for `time`, `org`,
 * and `hr` packs, following `registry/packs/locale-ar/labels.ts` (LABEL-LOCALE-1).
 *
 * Machine-proposed Arabic labels carry `needsReview: ["ar"]` until reviewed.
 */
import { Norn } from "@norn/authoring"

export const EgyptHrLabels = Norn.labels({
  id: "locale.eg_hr.labels",
  title: "Egyptian Arabic labels for time, org, and HR packs",
  version: "0.1.0",
  needsReview: ["ar"],
  types: {
    "time.Calendar": {
      label: { ar: "التقويم" },
      properties: {
        id: { ar: "المعرّف" },
        code: { ar: "الكود" },
        label: { ar: "الاسم" },
        iana_timezone: { ar: "المنطقة الزمنية" },
        dst_policy: { ar: "سياسة التوقيت الصيفي" },
        tzdb_version: { ar: "إصدار قاعدة التوقيت" },
        workweek_start: { ar: "بداية أسبوع العمل" },
      },
      values: {
        dst_policy: {
          reject: { ar: "رفض الغموض" },
          earlier: { ar: "الوقت الأسبق" },
          later: { ar: "الوقت الأحدث" },
        },
        workweek_start: {
          sunday: { ar: "الأحد" },
          monday: { ar: "الاثنين" },
          tuesday: { ar: "الثلاثاء" },
          wednesday: { ar: "الأربعاء" },
          thursday: { ar: "الخميس" },
          friday: { ar: "الجمعة" },
          saturday: { ar: "السبت" },
        },
      },
    },
    "time.CalendarException": {
      label: { ar: "استثناء تقويم" },
      properties: {
        id: { ar: "المعرّف" },
        calendar_date: { ar: "التاريخ" },
        kind: { ar: "النوع" },
        label: { ar: "الوصف" },
      },
      values: {
        kind: {
          holiday: { ar: "إجازة رسمية" },
          working_override: { ar: "يوم عمل استثنائي" },
          blackout: { ar: "فترة محظورة" },
        },
      },
    },
    "org.OrgUnit": {
      label: { ar: "وحدة تنظيمية / فرع" },
      properties: {
        id: { ar: "المعرّف" },
        display_name: { ar: "الاسم" },
        kind: { ar: "النوع" },
        unit_code: { ar: "كود الفرع" },
      },
    },
    "org.Post": {
      label: { ar: "وظيفة" },
      properties: {
        id: { ar: "المعرّف" },
        post_code: { ar: "كود الوظيفة" },
        title: { ar: "المسمى الوظيفي" },
        role_code: { ar: "الدور" },
        fte_capacity: { ar: "السعة الوظيفية" },
      },
    },
    "org.Membership": {
      label: { ar: "تعيين وظيفي" },
      properties: {
        id: { ar: "المعرّف" },
        role: { ar: "الدور" },
        valid_from: { ar: "ساري من" },
        valid_to: { ar: "ساري حتى" },
      },
    },
    "hr.Person": {
      label: { ar: "موظف" },
      properties: {
        id: { ar: "المعرّف" },
        display_name: { ar: "الاسم" },
        gender: { ar: "النوع" },
        national_id: { ar: "الرقم القومي" },
        phone: { ar: "رقم الهاتف" },
        primary_skill: { ar: "المهارة الأساسية" },
      },
      values: {
        gender: {
          female: { ar: "أنثى" },
          male: { ar: "ذكر" },
          unspecified: { ar: "غير محدد" },
        },
      },
    },
    "hr.Employment": {
      label: { ar: "شروط التوظيف" },
      properties: {
        id: { ar: "المعرّف" },
        effective_from: { ar: "تاريخ السريان" },
        effective_to: { ar: "تاريخ الانتهاء" },
        currency: { ar: "العملة" },
        base_salary: { ar: "الراتب الأساسي" },
        hourly_overtime_rate: { ar: "أجر الساعة الإضافية" },
        max_weekly_hours: { ar: "الحد الأقصى للساعات الأسبوعية" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          active: { ar: "نشط" },
          on_leave: { ar: "في إجازة" },
          terminated: { ar: "منتهي الخدمة" },
        },
      },
    },
    "hr.LeaveRequest": {
      label: { ar: "طلب إجازة" },
      properties: {
        id: { ar: "المعرّف" },
        leave_kind: { ar: "نوع الإجازة" },
        starts_on: { ar: "من تاريخ" },
        ends_on: { ar: "إلى تاريخ" },
        days: { ar: "عدد الأيام" },
        medical_note: { ar: "تقرير طبي" },
        status: { ar: "الحالة" },
      },
      values: {
        leave_kind: {
          annual: { ar: "سنوية" },
          sick: { ar: "مرضية" },
          unpaid: { ar: "بدون أجر" },
          emergency: { ar: "عارضة" },
        },
        status: {
          draft: { ar: "مسودة" },
          pending: { ar: "بانتظار الموافقة" },
          approved: { ar: "موافق عليه" },
          rejected: { ar: "مرفوض" },
          cancelled: { ar: "ملغى" },
        },
      },
    },
    "hr.Roster": {
      label: { ar: "جدول الورديات" },
      properties: {
        id: { ar: "المعرّف" },
        period_start: { ar: "بداية الفترة" },
        period_end: { ar: "نهاية الفترة" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          draft: { ar: "مسودة" },
          proposed: { ar: "مقترح" },
          published: { ar: "منشور" },
          superseded: { ar: "مُستبدَل" },
        },
      },
    },
    "hr.ShiftCommitment": {
      label: { ar: "وردية مجدولة" },
      properties: {
        id: { ar: "المعرّف" },
        starts_at: { ar: "بداية الوردية" },
        ends_at: { ar: "نهاية الوردية" },
        role_code: { ar: "الدور" },
        status: { ar: "الحالة" },
      },
    },
  },
})
