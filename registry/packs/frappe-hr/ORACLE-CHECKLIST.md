# Frappe HR v16 (`v16.60.0`) Black-Box Behavioral Oracle Checklist (#1592)

Clean-room behavioral edge-case checklist derived from public HR domain semantics
for black-box differential testing against a pinned Frappe HR v16 (`v16.60.0`,
commit `1b2d7e034e0de8e30a9e513541d7741710587ad5`) instance (`GPL-3.0-only`).
Removed legacy v15 concepts such as `Daily Work Summary` and `Identification Document Type`
(superseded by `Employee Document Type`) are intentionally excluded.
**Zero GPL-3.0 code, DocType JSON schemas, or field tables are copied into Norn.**

Evidence label: `declared` (specification checklist for black-box behavioral comparison;
not a claim of executed parity or runtime qualification).

This checklist explicitly separates:
- **Part A — Jurisdiction-Agnostic HR Domain Semantics** (universal conservation,
  immutability, exact-arithmetic, concurrency, access, and workflow invariants across all
  6 Frappe HR modules).
- **Part B — Country-Specific Statutory Localization (`loc-*`)** (jurisdictional labor
  law, social insurance, holiday calendars, and tax tables — exemplified by Egypt `loc-eg`
  under Labor Law 12/2003, Income Tax Law 91/2005, and NOSI Law 148/2019 — which MUST live
  in localization overlays rather than core HR definitions).

---

## Part A — Jurisdiction-Agnostic HR Domain Semantics

### 1. Core HR & Organization (`HR-CORE-*`)

| ID | Domain | Scenario | Required Norn Behavior | Notes / Intentional Divergence |
|---|---|---|---|---|
| `HR-CORE-01` | Core HR | Assign a Department's parent to itself or a descendant | Hard refusal; organizational unit graph remains strictly acyclic | Enforced structurally via `acyclic: true` on `org.OrgUnit.parent` |
| `HR-CORE-02` | Core HR | Employee receives a promotion or department/branch transfer effective mid-year | Prior `org.Membership` / `hr.Employment` closes at `effective_to`; new revision opens at `effective_from`; point-in-time historical queries stay exact | Frappe HR mutates `Employee` columns in place; Norn preserves bitemporal membership history |
| `HR-CORE-03` | Core HR | Attempt to submit a promotion, transfer, leave request, or shift assignment for an employee with status `Left` / inactive | Hard refusal with zero writes | Active employment precondition checked atomically at commit |
| `HR-CORE-04` | Core HR | Assign an employee as their own reporting manager or create a reporting loop (`A -> B -> A`) | Hard refusal; reporting hierarchy stays acyclic | Structural/law acyclicity check over reporting links |
| `HR-CORE-05` | Core HR | Submit and resolve an `Employee Grievance` involving confidential HR allegations | Grievance details carry restricted markings (`V2Markings`); resolution appends immutable disposition receipt rather than overwriting complaint history | Access scope enforced at query and commit, never UI-only |

### 2. Leave Management (`HR-LV-*`)

| ID | Domain | Scenario | Required Norn Behavior | Notes / Intentional Divergence |
|---|---|---|---|---|
| `HR-LV-01` | Leave | Pro-rata annual accrual mid-year | Exact `Decimal(2, "d")` allocation under declared rounding; never binary float | Norn refuses float rounding drift (`VAL-EXACT-1`) |
| `HR-LV-02` | Leave | Leave request spans a public holiday (`time.CalendarException`) | Holiday excluded from consumed leave days when calendar marks it non-working | Calendar exceptions supplied by pinned `time.Calendar` (country holidays live in `loc-*`) |
| `HR-LV-03` | Leave | Overlapping leave requests for the same employee | Second request refused atomically with zero partial hold/write | Enforced via `norn-zen/7` `intervals_overlap` + read fencing |
| `HR-LV-04` | Leave | Concurrent leave requests exceeding remaining balance (`balance = 3d`, two `2d` requests) | At most one `LeaveHold` commits; loser refuses cleanly | Reuses REA `core.Hold` conservation & CAS |
| `HR-LV-05` | Leave | Manager attempts to approve own `LeaveRequest` | Hard refusal under `AccessIR` / `#618` independent-approver law | Never weakened by UI |
| `HR-LV-06` | Leave | Carry-forward expiry at year boundary | Expired carry-forward days emit explicit adjustment event; history stays immutable | Bitemporal ledger, never mutates past balance |
| `HR-LV-07` | Leave | Leave request spans a blackout date on an active `Leave Block List` | Refused unless approved under an explicit authorized allow-list override with recorded evidence | Composes `time.CalendarException` (`blackout`) with governed HR policy |
| `HR-LV-08` | Leave | Employee submits `Compensatory Leave Request` without verified holiday/off-day attendance | Refused until linked to a posted `AttendanceEvent` on a non-working calendar day; prevents unbacked leave creation | Conservation law: every leave credit cites an authorized policy grant or verified holiday work event |
| `HR-LV-09` | Leave | `Leave Encashment` submitted for eligible unused leave balance | Atomically debits conserved leave balance (`Decimal(2, "d")`) and emits linked payroll payout claim; concurrent double-encashment refused | Two-sided conservation between leave balance and monetary claim |
| `HR-LV-10` | Leave | Leave type enables Frappe's `allow_negative` flag and request exceeds balance | Hard refusal unless an explicit governed leave-advance credit obligation exists | Intentional divergence (`non_conforming`): no silent unbacked negative balance bypass |

### 3. Shift Scheduling (`HR-SH-*`)

| ID | Domain | Scenario | Required Norn Behavior | Notes / Intentional Divergence |
|---|---|---|---|---|
| `HR-SH-01` | Shifts | Overlapping `ShiftCommitment` for one employee | Hard refusal at `publish_roster` commit | Enforced even if external solver proposes overlap |
| `HR-SH-02` | Shifts | Employee scheduled during `approved` `LeaveRequest` | Hard refusal at `publish_roster` commit | Fenced `SliceKey::Predicate` read over approved leaves |
| `HR-SH-03` | Shifts | Closing shift `23:00` → opening shift `06:00` (`7h < 11h` min rest) | Hard refusal (`interval_gap_seconds < min_rest_seconds`) | Generic rest invariant in core HR; numeric floor (`11h`, etc.) pinned by `loc-*` overlay |
| `HR-SH-04` | Shifts | Solver infeasible (short 1 cashier Saturday afternoon) | Solver returns typed `minimalRepairs[]` (`add_overtime`, `transfer_branch`, `reduce_coverage`) | No silent statutory relaxation; human chooses |
| `HR-SH-05` | Shifts | Overnight shift (`22:00` → `06:00`) crossing midnight or DST boundary | Materialized deterministically to exact half-open UTC `[starts_at, ends_at)` under pinned `time.Calendar` (`tzdb_version`, `dst_policy`) | Refuses ambiguous/nonexistent wall-clock times under `dst_policy: "reject"` (`TIME-PIN-1`) |
| `HR-SH-06` | Shifts | Approved `Shift Request` replaces an existing shift window | Atomically validates overlap, leave conflict, and minimum rest before committing successor `ShiftCommitment` | Never leaves orphaned or double-booked shift commitments |

### 4. Attendance & Time Capture (`HR-AT-*`)

| ID | Domain | Scenario | Required Norn Behavior | Notes / Intentional Divergence |
|---|---|---|---|---|
| `HR-AT-01` | Attendance | Retroactive attendance correction | New reversing `AttendanceEvent` linked via `reverses`; original event stays immutable | Intentional divergence (`non_conforming`): Frappe HR cancels and amends posted Attendance in place; Norn never edits posted history |
| `HR-AT-02` | Attendance | Attempt to post two active `AttendanceEvent`s for the same employee and shift/date | Second commit refused atomically | Enforced by uniqueness/overlap invariant |
| `HR-AT-03` | Attendance | Auto-attendance derived from raw `Employee Checkin` IN/OUT punches | Deterministic pairing from immutable `SourceEvidence` punches; exact UCUM hour computation; late-entry/early-exit flags preserve punch lineage | Reprocessing identical punch feed is replay-idempotent |
| `HR-AT-04` | Attendance | Approved `Attendance Request` regularizes a previously posted `Absent` or `Half Day` day | Appends linked reversing + replacement `AttendanceEvent` rather than mutating the original attendance row | Preserves full audit trail for payroll reconciliation |
| `HR-AT-05` | Attendance | Post `Present` attendance on a date already consumed by an approved full-day `LeaveRequest` | Hard refusal until leave is explicitly reversed/adjusted | Prevents double-counting worked pay and consumed leave on the same date |

### 5. Payroll, Compensation & Exit Settlement (`HR-PR-*`)

| ID | Domain | Scenario | Required Norn Behavior | Notes / Intentional Divergence |
|---|---|---|---|---|
| `HR-PR-01` | Payroll | Pro-rated monthly salary slip with unpaid leave (`LWP`) and mid-month join | Exact `Decimal` arithmetic (`VAL-EXACT-1`) under declared currency scale and rounding mode; `net_pay = gross_pay - total_deduction` holds exactly | Never uses binary64 floating point for pay components |
| `HR-PR-02` | Payroll | Component calculation using Frappe `Salary Component.formula` Python string | Refused in normal Norn profile; formulas must compile to typed declarative `RuleIR` / `ExpressionIR` (`Norn.expression` / `Norn.decision`) | Intentional divergence (`non_conforming`): zero runtime `eval` or host callbacks |
| `HR-PR-03` | Payroll | Attempt to submit two overlapping `Salary Slip`s for one employee in the same pay period | Hard refusal at commit with zero duplicate payroll accruals | Enforced by pay-period interval exclusivity |
| `HR-PR-04` | Payroll | Total deductions exceed gross earnings (`net_pay < 0`) | Hard refusal unless governed deferred-recovery / carry-forward deduction schedule is declared | Prevents unmodeled negative cash disbursements |
| `HR-PR-05` | Payroll | Retroactive payroll correction after slips and accrual journals have posted | Appends immutable retro-adjustment or reversing payroll events; prior posted payslip and ledger entries stay untouched | Intentional divergence (`non_conforming`): Frappe HR cancels and amends posted Salary Slips |
| `HR-PR-06` | Payroll | `Payroll Entry` submits slips and executes bank disbursement | Accrual transaction balances per currency (`Dr Payroll Expense / Cr Payroll Payable`); bank disbursement settles payable claim exactly once | Two-sided REA duality (`core.EconomicEvent` + `core.Claim`); never nets unlike currencies |
| `HR-PR-07` | Payroll | Effective-dated `Salary Structure Assignment` changes mid-period | Payroll evaluates effective-dated assignment splits at valid time without altering prior periods; missing active assignment refuses slip generation | Follows bitemporal setting/assignment rules (`D07`) |
| `HR-PR-08` | Payroll | `Full and Final Statement` submitted on employee separation | Atomically reconciles unpaid salary, leave encashment, gratuity, unrecovered advances/loans, and asset clearances into one balanced final claim | Prevents paying out separation settlement while open employee advances remain unoffset |

### 6. Expenses, Advances & Travel (`HR-EX-*`)

| ID | Domain | Scenario | Required Norn Behavior | Notes / Intentional Divergence |
|---|---|---|---|---|
| `HR-EX-01` | Expenses | Approver sets `sanctioned_amount > claimed_amount` on an `Expense Claim` | Hard refusal at commit | Exact decimal bound check per line and total |
| `HR-EX-02` | Expenses | Employee attempts to approve their own `Expense Claim` | Hard refusal under `AccessIR` / `#618` independent-approver law | Independent accountable identity required |
| `HR-EX-03` | Expenses | Allocate more `Employee Advance` balance to an `Expense Claim` than remaining unclaimed disbursed cash | Hard refusal (`claimed + returned <= paid <= approved`) | Conserved advance balance matching `allocation_exceeds_outstanding` |
| `HR-EX-04` | Expenses | Foreign-currency receipt attached to a local-currency `Expense Claim` or `Travel Request` | Requires explicit pinned FX conversion rate and date; refuses summing mixed currencies directly | Enforces per-currency ledger integrity |
| `HR-EX-05` | Expenses | Submit approved `Expense Claim` partially offset by an `Employee Advance` | Posts balanced accounting transaction (`Dr Expense / Cr Advance Asset + Cr Employee Payable`) with immutable receipt evidence links | Single atomic cell commit for expense recognition and advance offset |

### 7. Recruitment, Lifecycle & Performance (`HR-LC-*`)

| ID | Domain | Scenario | Required Norn Behavior | Notes / Intentional Divergence |
|---|---|---|---|---|
| `HR-LC-01` | Lifecycle | Initiate `Employee Onboarding` or issue an `Appointment Letter` from a revamped v16 `Job Offer` in `Awaiting Response` or `Rejected` state | Hard refusal; onboarding and formal appointment issuance require an accepted, unexpired `Job Offer` | Enforces recruitment-to-employment state transition order |
| `HR-LC-02` | Lifecycle | Re-hire a previously separated candidate via a new `Job Applicant` and `Job Offer` | Links a new `hr.Employment` / `org.Membership` to the existing `org.Person` identity without duplicating the person or overwriting prior employment tenure | Separates `org.Person` identity from `hr.Employment` intervals |
| `HR-LC-03` | Lifecycle | Complete `Employee Separation` while open advances, unreturned assets, or unsettled `Full and Final Statement` exist | Hard refusal (or explicit governed waiver receipt) until clearance obligations resolve | Prevents abandoned exit liabilities |
| `HR-LC-04` | Performance | Submit a second active `Appraisal` for the same employee in one `Appraisal Cycle` | Hard refusal at commit | Uniqueness invariant on `(employee, appraisal_cycle)` |
| `HR-LC-05` | Performance | Submit an `Appraisal` whose KRA or `Goal` weightages do not sum to `100%` | Hard refusal; composite score computed in exact `Decimal` arithmetic | Prevents silent score inflation or truncation |
| `HR-LC-06` | Learning | Submit `Training Result` recording hours and grade for a `Training Event` | Appends immutable completion evidence linked to attendee and `Training Event`; corrections append superseding records rather than deleting prior credentials | Preserves compliance training audit history |

### 8. Cross-Cutting Markings & Import Hygiene (`HR-PI-*`, `HR-IM-*`)

| ID | Domain | Scenario | Required Norn Behavior | Notes / Intentional Divergence |
|---|---|---|---|---|
| `HR-PI-01` | Markings | Branch manager or solver worker reads `hr.Person` / `hr.Employment` or attached `Employee Document Type` identity records | `gender`, `national_id`, `base_salary`, `medical_note`, and identity document attachments masked/redacted; reveal requires explicit role + writes receipt | `V2Markings` + `AccessIR` |
| `HR-IM-01` | Import | WhatsApp schedule screenshot #2 arrives 2h after screenshot #1 | Both `BlobRef`s retained; `FEED-CONFLICT` quarantine + Supersession Diff Card | Never silently overwrites prior screenshot |

---

## Part B — Country-Specific Statutory Localization Boundary (`loc-*`, Exemplified by `loc-eg`)

Jurisdiction-specific labor statutes, social insurance ceilings, tax brackets, and
official holiday calendars **must never be hardcoded inside jurisdiction-agnostic HR core
packs**. They compose as conjunctive, effective-dated localization overlays (`loc-*`)
pinned by organization/jurisdiction scope (`D07`, `D09`).

The table below isolates Egypt statutory rules (`loc-eg`) as the primary reference
localization boundary against Frappe HR's generic configuration tables:

| ID | Overlay | Statutory Basis | Required `loc-eg` Overlay Behavior | Separation from Core HR |
|---|---|---|---|---|
| `HR-LOC-EG-01` | `loc-eg` | Egypt Labor Law 12/2003 (and effective-dated successor statutory parameters) | Enforces statutory annual leave floors (`21d` standard after qualifying tenure; `30d` after 10 years insured service or age 50+), casual leave sub-cap, and statutory sick/maternity leave rules | Core HR owns generic conserved leave balances (`HR-LV-*`); `loc-eg` supplies statutory minimums and accrual parameters |
| `HR-LOC-EG-02` | `loc-eg` | Egypt Labor Law 12/2003 working-hours & rest provisions | Pins statutory daily/weekly rest floors, maximum ordinary working hours, overtime multipliers, and Ramadan/night-shift overlays | Core HR enforces generic rest-gap law (`HR-SH-03`); `loc-eg` supplies statutory numeric thresholds |
| `HR-LOC-EG-03` | `loc-eg` | Egypt official public holiday calendar & IANA `Africa/Cairo` tzdb | Pins `time.Calendar` with `iana_timezone: "Africa/Cairo"`, explicit `tzdb_version` & `dst_policy`, Friday/Saturday (or configured) weekly rest, and official national/religious `CalendarException` dates | Core HR owns `time.Calendar` resolution (`HR-LV-02`, `HR-SH-05`); `loc-eg` supplies Egypt calendar data |
| `HR-LOC-EG-04` | `loc-eg` | Egyptian Tax Authority (ETA) — Income Tax Law 91/2005 & effective-dated amendments | Evaluates progressive personal income tax brackets, personal exemption allowance, and tax-tier phase-outs via effective-dated `DecisionIR` / `SettingIR` tables (mapping Frappe HR `Income Tax Slab` + exemption proofs) | Core HR owns generic payslip deduction structure (`HR-PR-01`); `loc-eg` owns ETA tax law and filing outputs |
| `HR-LOC-EG-05` | `loc-eg` | Egypt NOSI — Social Insurance Law 148/2019 | Enforces effective-dated minimum/maximum insurable wage caps and employee/employer contribution rates with separate employer liability accruals | Frappe HR has no native NOSI engine (typically approximated via custom `Salary Component` formulas); `loc-eg` makes statutory caps declarative and versioned |
| `HR-LOC-EG-06` | `loc-eg` | Egypt Labor Law 12/2003 end-of-service gratuity & severance provisions | Computes statutory post-60 / termination indemnity schedules from verified tenure and effective-dated statutory rates before feeding `Full and Final Statement` | Core HR owns generic exit settlement reconciliation (`HR-PR-08`); `loc-eg` owns statutory indemnity formulas |
