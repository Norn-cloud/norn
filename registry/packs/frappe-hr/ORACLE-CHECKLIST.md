# Frappe HR Black-Box Behavioral Oracle Checklist (#1592)

Clean-room behavioral edge-case checklist derived from public HR domain semantics
for black-box differential testing against a pinned Frappe HR instance (GPL-3.0).
**Zero GPL code or DocType JSON schemas are copied into Norn.**

| ID | Domain | Scenario | Required Norn Behavior | Notes / Intentional Divergence |
|---|---|---|---|---|
| `HR-LV-01` | Leave | Pro-rata annual accrual mid-year | Exact `Decimal(2, "d")` allocation under declared rounding; never binary float | Norn refuses float rounding drift (`VAL-EXACT-1`) |
| `HR-LV-02` | Leave | Leave request spans a public holiday (`time.CalendarException`) | Holiday excluded from consumed leave days when calendar marks it non-working | Uses pinned `time.Calendar` (`loc-eg`) |
| `HR-LV-03` | Leave | Overlapping leave requests for the same employee | Second request refused atomically with zero partial hold/write | Enforced via `norn-zen/7` `intervals_overlap` + read fencing |
| `HR-LV-04` | Leave | Concurrent leave requests exceeding remaining balance (`balance = 3d`, two `2d` requests) | At most one `LeaveHold` commits; loser refuses cleanly | Reuses REA `core.Hold` conservation & CAS |
| `HR-LV-05` | Leave | Manager attempts to approve own `LeaveRequest` | Hard refusal under `AccessIR` / `#618` independent-approver law | Never weakened by UI |
| `HR-LV-06` | Leave | Carry-forward expiry at year boundary | Expired carry-forward days emit explicit adjustment event; history stays immutable | Bitemporal ledger, never mutates past balance |
| `HR-SH-01` | Shifts | Overlapping `ShiftCommitment` for one employee | Hard refusal at `publish_roster` commit | Enforced even if external solver proposes overlap |
| `HR-SH-02` | Shifts | Employee scheduled during `approved` `LeaveRequest` | Hard refusal at `publish_roster` commit | Fenced `SliceKey::Predicate` read over approved leaves |
| `HR-SH-03` | Shifts | Closing shift `23:00` → opening shift `06:00` (`7h < 11h` min rest) | Hard refusal (`interval_gap_seconds < min_rest_seconds`) | Statutory floor from `loc-eg` overlay |
| `HR-SH-04` | Shifts | Solver infeasible (short 1 cashier Saturday afternoon) | Solver returns typed `minimalRepairs[]` (`add_overtime`, `transfer_branch`, `reduce_coverage`) | No silent statutory relaxation; human chooses |
| `HR-AT-01` | Attendance | Retroactive attendance correction | New reversing `AttendanceEvent` linked via `reverses`; original event stays immutable | Never edits posted history |
| `HR-PI-01` | Markings | Branch manager or solver worker reads `hr.Person` / `hr.Employment` | `gender`, `national_id`, `base_salary`, `medical_note` masked/redacted; reveal requires explicit role + writes receipt | `V2Markings` + `AccessIR` |
| `HR-IM-01` | Import | WhatsApp schedule screenshot #2 arrives 2h after screenshot #1 | Both `BlobRef`s retained; `FEED-CONFLICT` quarantine + Supersession Diff Card | Never silently overwrites prior screenshot |
