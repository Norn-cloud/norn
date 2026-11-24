/**
 * Egypt localization pack (`loc-eg`): Egyptian Arabic labels for `time`, `org`,
 * `hr`, and `loc_eg` packs (Egypt Labor Law No. 14 of 2025), following
 * `registry/packs/locale-ar/labels.ts` (LABEL-LOCALE-1).
 *
 * Machine-proposed Arabic labels carry `needsReview: ["ar"]` until reviewed.
 */
import { Norn } from "@norn/authoring"

export const EgyptHrLabels = Norn.labels({
  id: "locale.eg_hr.labels",
  title: "Egyptian Arabic labels for time, org, HR, and Egypt localization packs",
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
        jurisdiction_code: { ar: "كود الدولة / الاختصاص" },
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
    "hr.LeavePolicy": {
      label: { ar: "سياسة الإجازات" },
      properties: {
        id: { ar: "المعرّف" },
        policy_code: { ar: "كود السياسة" },
        label: { ar: "اسم السياسة" },
        annual_base_days: { ar: "رصيد الإجازة السنوية" },
        sick_base_days: { ar: "رصيد الإجازة المرضية" },
        emergency_base_days: { ar: "رصيد الإجازة العارضة" },
        max_carry_forward_days: { ar: "الحد الأقصى للترحيل" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          active: { ar: "نشط" },
          archived: { ar: "مؤرشف" },
        },
      },
    },
    "hr.LeaveAllocation": {
      label: { ar: "تخصيص إجازة" },
      properties: {
        id: { ar: "المعرّف" },
        jurisdiction_code: { ar: "كود الدولة / الاختصاص" },
        leave_kind: { ar: "نوع الإجازة" },
        entitlement_tier: { ar: "فئة الاستحقاق" },
        hazardous_or_remote: { ar: "أعمال خطرة أو مناطق نائية" },
        starts_on: { ar: "من تاريخ" },
        ends_on: { ar: "إلى تاريخ" },
        allocated_days: { ar: "الأيام المخصصة" },
        carry_forward_days: { ar: "الأيام المرحّلة" },
        status: { ar: "الحالة" },
      },
      values: {
        leave_kind: {
          annual: { ar: "سنوية" },
          sick: { ar: "مرضية" },
          unpaid: { ar: "بدون أجر" },
          emergency: { ar: "عارضة" },
          compensatory: { ar: "تعويضية" },
        },
        entitlement_tier: {
          prorated_initial: { ar: "نسبة وتناسب (أقل من سنة)" },
          first_year: { ar: "السنة الأولى" },
          standard: { ar: "اعتيادي (السنة الثانية فأكثر)" },
          senior: { ar: "أقدمية (١٠ سنوات أو ٥٠ عاماً)" },
          protected_disability: { ar: "ذوو الإعاقة / قصار القامة" },
        },
        status: {
          draft: { ar: "مسودة" },
          active: { ar: "نشط" },
          expired: { ar: "منتهي" },
          cancelled: { ar: "ملغى" },
        },
      },
    },
    "hr.LeaveRequest": {
      label: { ar: "طلب إجازة" },
      properties: {
        id: { ar: "المعرّف" },
        requester_user_id: { ar: "مقدم الطلب" },
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
          compensatory: { ar: "تعويضية" },
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
    "hr.CompensatoryLeaveRequest": {
      label: { ar: "طلب إجازة تعويضية" },
      properties: {
        id: { ar: "المعرّف" },
        requester_user_id: { ar: "مقدم الطلب" },
        work_date: { ar: "تاريخ العمل" },
        reason: { ar: "السبب" },
        credited_days: { ar: "الأيام المستحقة" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          draft: { ar: "مسودة" },
          pending: { ar: "بانتظار الموافقة" },
          approved: { ar: "موافق عليه" },
          rejected: { ar: "مرفوض" },
        },
      },
    },
    "hr.LeaveEncashment": {
      label: { ar: "مقابل نقدي للإجازات" },
      properties: {
        id: { ar: "المعرّف" },
        requester_user_id: { ar: "مقدم الطلب" },
        encashment_date: { ar: "تاريخ الصرف" },
        encashable_days: { ar: "الأيام المستحقة للصرف" },
        currency: { ar: "العملة" },
        encashment_amount: { ar: "قيمة المقابل النقدي" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          draft: { ar: "مسودة" },
          approved: { ar: "موافق عليه" },
          paid: { ar: "تم الصرف" },
          cancelled: { ar: "ملغى" },
        },
      },
    },
    "hr.LaborCapacity": {
      label: { ar: "سعة العمل" },
      properties: {
        id: { ar: "المعرّف" },
        resource_code: { ar: "كود المورد" },
        quantity: { ar: "عدد الساعات" },
        role_code: { ar: "الدور" },
        period_code: { ar: "الفترة" },
      },
    },
    "hr.PreferenceProfile": {
      label: { ar: "ملف تفضيلات الجدولة" },
      properties: {
        id: { ar: "المعرّف" },
        jurisdiction_code: { ar: "كود الدولة / الاختصاص" },
        label: { ar: "الاسم" },
        max_daily_hours: { ar: "الحد الأقصى للساعات اليومية" },
        max_weekly_hours: { ar: "الحد الأقصى للساعات الأسبوعية" },
        min_weekly_rest_hours: { ar: "الحد الأدنى للراحة الأسبوعية" },
        max_consecutive_days: { ar: "الحد الأقصى لأيام العمل المتتالية" },
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
        timezone: { ar: "المنطقة الزمنية" },
        role_code: { ar: "الدور" },
        status: { ar: "الحالة" },
      },
    },
    "hr.AttendanceEvent": {
      label: { ar: "سجل حضور وانصراف" },
      properties: {
        id: { ar: "المعرّف" },
        starts_at: { ar: "وقت الحضور" },
        ends_at: { ar: "وقت الانصراف" },
        timezone: { ar: "المنطقة الزمنية" },
        quantity: { ar: "ساعات العمل" },
      },
    },
    "hr.AttendanceRequest": {
      label: { ar: "طلب تسوية حضور" },
      properties: {
        id: { ar: "المعرّف" },
        requester_user_id: { ar: "مقدم الطلب" },
        work_date: { ar: "تاريخ العمل" },
        reason: { ar: "السبب" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          draft: { ar: "مسودة" },
          pending: { ar: "بانتظار الموافقة" },
          approved: { ar: "موافق عليه" },
          rejected: { ar: "مرفوض" },
        },
      },
    },
    "hr.PayrollFund": {
      label: { ar: "صندوق الرواتب" },
      properties: {
        id: { ar: "المعرّف" },
        resource_code: { ar: "كود الصندوق" },
        quantity: { ar: "الرصيد" },
        currency: { ar: "العملة" },
      },
    },
    "hr.SalaryStructure": {
      label: { ar: "هيكل الرواتب" },
      properties: {
        id: { ar: "المعرّف" },
        structure_code: { ar: "كود الهيكل" },
        label: { ar: "الاسم" },
        currency: { ar: "العملة" },
        pay_frequency: { ar: "دورية الصرف" },
        status: { ar: "الحالة" },
      },
      values: {
        pay_frequency: {
          monthly: { ar: "شهري" },
          biweekly: { ar: "كل أسبوعين" },
          weekly: { ar: "أسبوعي" },
        },
        status: {
          active: { ar: "نشط" },
          archived: { ar: "مؤرشف" },
        },
      },
    },
    "hr.SalaryStructureAssignment": {
      label: { ar: "تخصيص هيكل راتب" },
      properties: {
        id: { ar: "المعرّف" },
        effective_from: { ar: "تاريخ السريان" },
        currency: { ar: "العملة" },
        base_pay: { ar: "الأجر الأساسي" },
      },
    },
    "hr.PayrollRun": {
      label: { ar: "مسير الرواتب" },
      properties: {
        id: { ar: "المعرّف" },
        period_code: { ar: "كود الفترة" },
        starts_at: { ar: "بداية الفترة" },
        ends_at: { ar: "نهاية الفترة" },
        currency: { ar: "العملة" },
        total_gross: { ar: "إجمالي الاستحقاقات" },
        total_deductions: { ar: "إجمالي الاستقطاعات" },
        total_net: { ar: "صافي المسير" },
        prepared_by_user_id: { ar: "مُعد المسير" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          draft: { ar: "مسودة" },
          calculated: { ar: "محسوب" },
          approved: { ar: "معتمد" },
          posted: { ar: "مُرحّل" },
        },
      },
    },
    "hr.SalarySlip": {
      label: { ar: "قسيمة راتب" },
      properties: {
        id: { ar: "المعرّف" },
        gross_pay: { ar: "إجمالي الراتب" },
        total_deductions: { ar: "إجمالي الخصومات" },
        net_pay: { ar: "صافي الراتب" },
        status: { ar: "الحالة" },
      },
    },
    "hr.EmployeeAdvance": {
      label: { ar: "عهدة / سلفة موظف" },
      properties: {
        id: { ar: "المعرّف" },
        amount: { ar: "المبلغ" },
        currency: { ar: "العملة" },
        due_date: { ar: "تاريخ الاستحقاق" },
        requester_user_id: { ar: "مقدم الطلب" },
        purpose: { ar: "الغرض" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          draft: { ar: "مسودة" },
          approved: { ar: "معتمد" },
          disbursed: { ar: "مصروف" },
          settled: { ar: "مُسوّى" },
          cancelled: { ar: "ملغى" },
        },
      },
    },
    "hr.ExpenseClaim": {
      label: { ar: "مطالبة مصروفات" },
      properties: {
        id: { ar: "المعرّف" },
        requester_user_id: { ar: "مقدم الطلب" },
        claimed_amount: { ar: "المبلغ المطلوب" },
        sanctioned_amount: { ar: "المبلغ المعتمد" },
        currency: { ar: "العملة" },
        description: { ar: "الوصف" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          draft: { ar: "مسودة" },
          submitted: { ar: "مُقدّم" },
          approved: { ar: "معتمد" },
          paid: { ar: "مدفوع" },
          rejected: { ar: "مرفوض" },
        },
      },
    },
    "hr.JobOpening": {
      label: { ar: "وظيفة شاغرة" },
      properties: {
        id: { ar: "المعرّف" },
        title: { ar: "المسمى الوظيفي" },
        openings_count: { ar: "عدد الشواغر" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          open: { ar: "مفتوحة" },
          on_hold: { ar: "معلّقة" },
          closed: { ar: "مغلقة" },
        },
      },
    },
    "hr.JobApplicant": {
      label: { ar: "متقدم لوظيفة" },
      properties: {
        id: { ar: "المعرّف" },
        full_name: { ar: "الاسم الكامل" },
        email: { ar: "البريد الإلكتروني" },
        phone: { ar: "رقم الهاتف" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          applied: { ar: "تقدّم" },
          screening: { ar: "فرز أولي" },
          interviewed: { ar: "تمت المقابلة" },
          offered: { ar: "عرض وظيفي" },
          hired: { ar: "تم التعيين" },
          rejected: { ar: "مرفوض" },
          withdrawn: { ar: "منسحب" },
        },
      },
    },
    "hr.JobOffer": {
      label: { ar: "عرض عمل" },
      properties: {
        id: { ar: "المعرّف" },
        applicant_user_id: { ar: "معرّف المرشح" },
        currency: { ar: "العملة" },
        offered_ctc: { ar: "إجمالي العرض السنوي" },
        offer_date: { ar: "تاريخ العرض" },
        expiry_date: { ar: "تاريخ الانتهاء" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          draft: { ar: "مسودة" },
          sent: { ar: "مُرسل" },
          accepted: { ar: "مقبول" },
          declined: { ar: "مرفوض" },
          expired: { ar: "منتهي الصلاحية" },
        },
      },
    },
    "hr.OnboardingChecklist": {
      label: { ar: "قائمة إجراءات التعيين" },
      properties: {
        id: { ar: "المعرّف" },
        required_items_count: { ar: "البنود المطلوبة" },
        completed_items_count: { ar: "البنود المكتملة" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          in_progress: { ar: "قيد التنفيذ" },
          completed: { ar: "مكتملة" },
          cancelled: { ar: "ملغاة" },
        },
      },
    },
    "hr.Appraisal": {
      label: { ar: "تقييم الأداء" },
      properties: {
        id: { ar: "المعرّف" },
        cycle_code: { ar: "دورة التقييم" },
        score: { ar: "الدرجة" },
        status: { ar: "الحالة" },
      },
      values: {
        status: {
          draft: { ar: "مسودة" },
          submitted: { ar: "مُقدّم" },
          finalized: { ar: "نهائي" },
        },
      },
    },
    "loc_eg.LaborProfile": {
      label: { ar: "ملف قانون العمل المصري (قانون ١٤ لسنة ٢٠٢٥)" },
      properties: {
        id: { ar: "المعرّف" },
        effective_from: { ar: "تاريخ السريان" },
        iana_timezone: { ar: "المنطقة الزمنية" },
        currency: { ar: "العملة" },
        max_regular_weekly_hours: { ar: "الحد الأقصى للساعات الأسبوعية" },
        max_daily_hours: { ar: "الحد الأقصى لساعات العمل اليومي" },
        max_workplace_presence_hours: { ar: "الحد الأقصى لساعات التواجد اليومي" },
        min_weekly_rest_hours: { ar: "الحد الأدنى للراحة الأسبوعية" },
        first_year_annual_leave_days: { ar: "الإجازة السنوية للسنة الأولى" },
        standard_annual_leave_days: { ar: "الإجازة السنوية الاعتيادية" },
        senior_annual_leave_days: { ar: "الإجازة السنوية للأقدمية" },
        disability_annual_leave_days: { ar: "الإجازة السنوية لذوي الإعاقة" },
        hazardous_remote_supplement_days: { ar: "الأيام الإضافية للأعمال الخطرة أو النائية" },
        seniority_threshold_years: { ar: "سنوات الأقدمية" },
        weekend_pattern: { ar: "نظام الراحة الأسبوعية" },
      },
      values: {
        weekend_pattern: {
          friday_saturday: { ar: "الجمعة والسبت" },
          friday_only: { ar: "الجمعة فقط" },
        },
      },
    },
  },
})
