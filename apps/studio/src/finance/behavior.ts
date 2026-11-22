import type { FinanceType, MarkId } from "./data.ts"

export const VALUE_OBJECTS = {
  Money: "An amount in minor units plus an ISO currency code. This workspace uses EGP.",
  NationalId: "14 digits with a checksum. Synthetic records display a demo token in place of the number.",
  IScore: "An integer from 400 to 850.",
  DPD: "A whole number of calendar days past the installment due date.",
  Percent: "A percentage from 0 to 100.",
} as const

export type ValueObject = keyof typeof VALUE_OBJECTS
export type RuleId = "kyc" | "exposure" | "settlement" | "hardship" | "margin" | "identity" | "aml" | "reporting"

type EventPayloadField = readonly [name: string, type: ValueObject | "Reference" | "Date" | "Text"]
type EventDefinition = {
  readonly on: FinanceType
  readonly payload: readonly EventPayloadField[]
  readonly reactions: readonly string[]
}

export const EVENTS = {
  KycExpired: { on: "kyc-check", payload: [["clientId", "Reference"], ["verificationId", "Reference"], ["expiredOn", "Date"]],
    reactions: ["Loan origination blocks disbursement.", "Brokerage reopens identity review if an application is pending."] },
  LoanApproved: { on: "loan", payload: [["loanId", "Reference"], ["principal", "Money"], ["approvedBy", "Reference"], ["scoreAtDecision", "IScore"]],
    reactions: ["Origination prepares a disbursement and checks fresh evidence again."] },
  LoanDisbursed: { on: "loan", payload: [["loanId", "Reference"], ["paymentRef", "Reference"], ["amount", "Money"], ["valueDate", "Date"]],
    reactions: ["Loan servicing opens the installment schedule.", "FRA reporting includes the posted disbursement."] },
  LoanArrearsChanged: { on: "loan", payload: [["loanId", "Reference"], ["clientId", "Reference"], ["dpd", "DPD"]],
    reactions: ["Brokerage hold recalculates whether to block new margin and cash withdrawals."] },
  InstallmentMissed: { on: "installment", payload: [["loanId", "Reference"], ["installmentId", "Reference"], ["balanceDue", "Money"], ["dpd", "DPD"]],
    reactions: ["The collections process checks the Client's hardship case.", "The brokerage hold process watches the arrears threshold."] },
  SettlementReceived: { on: "payment", payload: [["paymentRef", "Reference"], ["payerRef", "Reference"], ["amount", "Money"], ["valueDate", "Date"]],
    reactions: ["Settlement handling searches for the LMS installment.", "Repayment reconciliation rechecks the reporting period."] },
  SettlementReclassified: { on: "payment", payload: [["paymentRef", "Reference"], ["amount", "Money"], ["originalWeek", "Date"], ["correctedWeek", "Date"]],
    reactions: ["Repayment reconciliation revises the published value and retains the value previously known."] },
  HardshipOpened: { on: "hardship-case", payload: [["caseId", "Reference"], ["clientId", "Reference"], ["openedOn", "Date"], ["reason", "Text"]],
    reactions: ["Collections pauses dunning.", "Loan servicing opens a restructuring review."] },
  LoanRestructured: { on: "loan", payload: [["loanId", "Reference"], ["caseId", "Reference"], ["scheduleVersion", "Text"]],
    reactions: ["Collections follows the revised installment schedule."] },
  LoanClosed: { on: "loan", payload: [["loanId", "Reference"], ["settlementRef", "Reference"], ["closedOn", "Date"]],
    reactions: ["Credit exposure is recalculated.", "Brokerage rechecks the hold."] },
  MarginOrderPlaced: { on: "order", payload: [["orderId", "Reference"], ["accountId", "Reference"], ["creditUsed", "Money"]],
    reactions: ["Brokerage reserves available margin.", "Credit exposure is recomputed on the Client."] },
  BrokerageHoldPlaced: { on: "brokerage-account", payload: [["accountId", "Reference"], ["loanId", "Reference"], ["dpd", "DPD"]],
    reactions: ["Brokerage stops new margin orders and cash withdrawals."] },
} as const satisfies Record<string, EventDefinition>

export type FinanceEvent = keyof typeof EVENTS

type CommandDefinition = {
  readonly on: FinanceType
  readonly issuer: string
  readonly guard: string
  readonly rules: readonly RuleId[]
  readonly emits: readonly FinanceEvent[]
}

export const COMMANDS = {
  "Loan.approve": { on: "loan", issuer: "Credit officer or underwriting service",
    guard: "Auto-approve at I-Score ≥ 650. At 600–649 require a credit officer's decision. DTI ≤ 50%, no identity conflict, and exposure ≤ EGP 5M.",
    rules: ["identity", "exposure"], emits: ["LoanApproved"] },
  "Loan.disburse": { on: "loan", issuer: "Disbursement officer",
    guard: "The loan is approved, rule 1 passes, and two distinct approvers are recorded above EGP 1M.",
    rules: ["kyc"], emits: ["LoanDisbursed"] },
  "Loan.restructure": { on: "loan", issuer: "Collections process or credit officer",
    guard: "An open hardship case refers to the borrower and the revised schedule has been approved.",
    rules: ["hardship"], emits: ["LoanRestructured"] },
  "Loan.settle": { on: "loan", issuer: "Loan servicing",
    guard: "The settlement is matched and outstanding principal and fees are zero, including early repayment.",
    rules: ["settlement"], emits: ["LoanClosed"] },
  "Loan.writeOff": { on: "loan", issuer: "Credit committee",
    guard: "The loan is more than 90 DPD and the committee has recorded its decision.",
    rules: [], emits: [] },
  "Installment.pauseDunning": { on: "installment", issuer: "Collections process",
    guard: "The CRM hardship case is open for the installment's borrower.",
    rules: ["hardship"], emits: [] },
  "Installment.dun": { on: "installment", issuer: "Collections service",
    guard: "The installment is past due and the client has no open hardship case.",
    rules: ["hardship"], emits: [] },
  "Installment.lateFee": { on: "installment", issuer: "Loan servicing",
    guard: "The installment is past due and the client has no open hardship case.",
    rules: ["hardship"], emits: [] },
  "PromiseToPay.request": { on: "installment", issuer: "Collections process",
    guard: "The balance is overdue and the CRM hardship check is clear.",
    rules: ["hardship"], emits: [] },
  "HardshipCase.open": { on: "hardship-case", issuer: "Relationship manager",
    guard: "A borrower request is linked to an outstanding loan.",
    rules: [], emits: ["HardshipOpened"] },
  "Order.placeMargin": { on: "order", issuer: "Brokerage execution service",
    guard: "The account is suitable for margin, the borrower is below 30 DPD, and exposure stays within policy.",
    rules: ["margin", "exposure"], emits: ["MarginOrderPlaced"] },
  "BrokerageAccount.withdraw": { on: "brokerage-account", issuer: "Brokerage payments service",
    guard: "Funds are available and the client is below 30 DPD.",
    rules: ["margin"], emits: [] },
  "BrokerageAccount.blockMargin": { on: "brokerage-account", issuer: "Brokerage hold process",
    guard: "The Brokerage account and overdue Loan resolve to the same Client at 30 or more DPD.",
    rules: ["margin"], emits: ["BrokerageHoldPlaced"] },
  "Payment.receive": { on: "payment", issuer: "Payments adapter",
    guard: "Fawry or InstaPay supplied a settlement reference, currency, amount and value date.",
    rules: ["settlement"], emits: ["SettlementReceived"] },
  "Payment.reclassify": { on: "payment", issuer: "Reconciliation analyst",
    guard: "Bank evidence shows a different reporting period. Preserve the earlier reading.",
    rules: ["settlement"], emits: ["SettlementReclassified"] },
  "RegulatoryReturn.file": { on: "regulatory-return", issuer: "Compliance officer",
    guard: "LMS, Brokerage and general-ledger totals reconcile to the same reporting period.",
    rules: ["reporting"], emits: [] },
} as const satisfies Record<string, CommandDefinition>

export type FinanceCommand = keyof typeof COMMANDS
export const isFinanceCommand = (value: string): value is FinanceCommand => Object.hasOwn(COMMANDS, value)

export const AGGREGATES = [
  { root: "client", members: ["kyc-check"], related: ["relationship-assignment"] },
  { root: "loan", members: ["installment"], related: [] },
  { root: "brokerage-account", members: ["order"], related: ["holdings"] },
] as const satisfies readonly { root: FinanceType; members: readonly FinanceType[]; related: readonly string[] }[]

export const AUXILIARY_TYPES = {
  "relationship-assignment": { name: "Relationship assignment", parent: "client", source: "salesforce",
    fields: ["Client reference", "Relationship manager reference", "Assigned on", "Assignment status"] },
  holdings: { name: "Holdings", parent: "brokerage-account", source: "mcdr",
    fields: ["Account number", "Instrument reference", "Position value (EGP)", "As-of date"] },
} as const satisfies Record<string, { name: string; parent: FinanceType; source: MarkId; fields: readonly string[] }>

export type AuxiliaryType = keyof typeof AUXILIARY_TYPES

export type State = {
  readonly name: string
  readonly kind: "initial" | "active" | "review" | "terminal" | "refused"
  readonly column: number
  readonly row: number
}

export type StateStep<Key extends string = string> = {
  readonly from: Key
  readonly to: Key
  readonly command: string
  readonly guard: string
  readonly systems: readonly [MarkId, MarkId, ...MarkId[]]
  readonly needs: string
  readonly declared?: FinanceCommand
}

export interface StatePhase {
  readonly title: string
  readonly states: Readonly<Record<string, State>>
  readonly steps: readonly StateStep[]
}

function phase<const S extends Readonly<Record<string, State>>>(title: string, states: S, steps: readonly StateStep<keyof S & string>[]): StatePhase {
  return { title, states, steps }
}

function state(name: string, column: number, row: number, kind: State["kind"] = "active"): State {
  return { name, column, row, kind }
}

function step<Key extends string>(from: Key, to: Key, command: string, guard: string, systems: StateStep["systems"], needs: string, declared?: FinanceCommand): StateStep<Key> {
  return { from, to, command, guard, systems, needs, ...(declared ? { declared } : {}) }
}

const kycSystems = ["valify", "salesforce"] as const
const creditSystems = ["iscore", "salesforce", "postgres"] as const
const disbursementSystems = ["postgres", "valify", "iscore", "salesforce", "instapay"] as const
const repaymentSystems = ["postgres", "fawry", "instapay"] as const
const hardshipSystems = ["salesforce", "postgres"] as const
const brokerageSystems = ["valify", "mcdr"] as const
const fundingSystems = ["mcdr", "instapay"] as const

const origination = phase("Origination", {
  applied: state("Applied", 0, 0, "initial"),
  verified: state("KYC verified", 1, 0),
  kycFailed: state("KYC failed", 1, 1, "review"),
  scored: state("I-Score pulled", 2, 0),
  review: state("Manual review", 3, 1, "review"),
  approved: state("Approved", 4, 0),
  declined: state("Declined", 4, 2, "refused"),
  disbursed: state("Disbursed", 5, 0),
}, [
  step("applied", "verified", "verifyKyc", "Identity verified", kycSystems, "Compare the CRM applicant to Valify identity and liveness evidence."),
  step("applied", "kycFailed", "verifyKyc", "Identity check failed", kycSystems, "Store the Valify failure against the original CRM applicant."),
  step("kycFailed", "applied", "retryKyc", "Retry count ≤ 2", kycSystems, "Read retry history from CRM before starting another Valify check."),
  step("kycFailed", "declined", "decline", "Retries exhausted", kycSystems, "Record the exhausted Valify checks against the CRM application."),
  step("verified", "scored", "pullIScore", "Inquiry ≤ 30 days", ["valify", "iscore"], "Match the bureau inquiry to the verified national ID."),
  step("scored", "approved", "autoApprove", "Score ≥ 650 · DTI ≤ 50%", creditSystems, "Combine bureau score, LMS affordability and identity clearance.", "Loan.approve"),
  step("scored", "review", "review", "Score 600–649", creditSystems, "Open a CRM review with the bureau score and the LMS application."),
  step("scored", "declined", "decline", "Score < 600", creditSystems, "Close the LMS application with the I-Score result and retain the CRM decision."),
  step("review", "approved", "approve", "Officer approves · DTI ≤ 50%", creditSystems, "Match the officer's decision in CRM to the bureau evidence and LMS affordability.", "Loan.approve"),
  step("review", "declined", "decline", "Officer declines", creditSystems, "Record the officer's decision across the LMS and CRM."),
  step("approved", "disbursed", "disburse", "Rule 1 · two approvers above EGP 1M", disbursementSystems, "LMS approval, Valify evidence, I-Score and CRM clearance gate the InstaPay release.", "Loan.disburse"),
])

const servicing = phase("Servicing", {
  disbursed: state("Disbursed", 0, 0, "initial"),
  performing: state("Performing", 0, 1),
  arrears: state("In arrears · 1–30 DPD", 1, 1),
  late: state("31–90 DPD", 2, 1),
  restructured: state("Restructured", 3, 0, "review"),
  promise: state("Promise to pay", 3, 1, "review"),
  escalation: state("Collections escalation", 4, 1, "review"),
  severe: state("Over 90 DPD", 5, 1, "review"),
  writtenOff: state("Written off", 5, 2, "terminal"),
  closed: state("Closed", 2, 2, "terminal"),
}, [
  step("disbursed", "performing", "activate", "Disbursement settled", ["postgres", "instapay"], "Activate the LMS schedule after InstaPay confirms the release."),
  step("performing", "arrears", "missInstallment", "1–30 DPD", repaymentSystems, "Confirm the LMS due amount is still uncovered by bank settlements."),
  step("arrears", "performing", "applyPayment", "Overdue balance cleared", repaymentSystems, "Match the bank payment before clearing the LMS overdue balance."),
  step("arrears", "late", "advanceArrears", "Still unpaid on day 31", repaymentSystems, "Recompute DPD after applying confirmed Fawry and InstaPay settlements."),
  step("late", "restructured", "openHardship", "CRM case open · schedule approved", hardshipSystems, "CRM owns the hardship case and the LMS owns the revised schedule.", "Loan.restructure"),
  step("restructured", "performing", "activatePlan", "Revised terms in force", hardshipSystems, "Activate the approved LMS schedule for the same CRM hardship case."),
  step("late", "promise", "promiseToPay", "No open hardship case", hardshipSystems, "Check CRM hardship status before the LMS requests a promise.", "PromiseToPay.request"),
  step("promise", "performing", "keepPromise", "Matched payment received", repaymentSystems, "A bank settlement must match the promised installment."),
  step("promise", "escalation", "breakPromise", "Promise passed without payment", repaymentSystems, "Check both the LMS promise date and bank evidence before escalating."),
  step("escalation", "severe", "advanceArrears", "Overdue > 90 DPD", repaymentSystems, "Check LMS arrears against bank settlements before crossing 90 days."),
  step("late", "severe", "advanceArrears", "No plan or promise · > 90 DPD", repaymentSystems, "Check the current installment balance against the latest settlements."),
  step("severe", "writtenOff", "writeOff", "Credit committee decision recorded", hardshipSystems, "CRM records the committee decision before the LMS posts the write-off.", "Loan.writeOff"),
  step("performing", "closed", "settle", "Final installment matched", repaymentSystems, "The final payment must match and close the LMS balance.", "Loan.settle"),
  step("performing", "closed", "earlyRepay", "Early payoff matched · balance zero", repaymentSystems, "Verify the payoff amount, value date and LMS close-out balance.", "Loan.settle"),
])

const onboarding = phase("Onboarding", {
  ekyc: state("e-KYC", 0, 0, "initial"),
  suitability: state("Suitability", 1, 0),
  identity: state("Identity review", 1, 1, "review"),
  margin: state("Margin account", 2, 0),
  cash: state("Cash account", 2, 1),
  rejected: state("Rejected", 2, 2, "refused"),
  registered: state("MCDR registered", 3, 0),
  funded: state("Funded", 4, 0, "terminal"),
}, [
  step("ekyc", "suitability", "assessSuitability", "KYC verified", kycSystems, "A verified Valify identity must match the CRM applicant."),
  step("ekyc", "identity", "requestIdentityReview", "KYC mismatch", kycSystems, "Capture conflicting CRM and Valify identity evidence for review."),
  step("identity", "rejected", "reject", "Mismatch unresolved", kycSystems, "Record an identity refusal against the CRM application."),
  step("identity", "suitability", "resume", "Identity resolved", kycSystems, "Return to suitability after CRM and Valify agree."),
  step("suitability", "margin", "openMargin", "Suitable · I-Score ≥ 650", ["iscore", "valify", "egx"], "Combine the bureau score, verified identity and broker suitability."),
  step("suitability", "cash", "openCash", "Cash only or score < 650", ["iscore", "valify", "mcdr"], "Attach the regulated cash-account option to the verified client and score."),
  step("margin", "registered", "registerMcdr", "Margin terms accepted", brokerageSystems, "MCDR registers the account against a verified owner."),
  step("cash", "registered", "registerMcdr", "Cash terms accepted", brokerageSystems, "MCDR registers the account against a verified owner."),
  step("registered", "funded", "confirmFunding", "InstaPay funds matched", fundingSystems, "The deposit must match the account number acknowledged by MCDR."),
])

const settlements = phase("Settlement handling", {
  received: state("Received", 0, 1, "initial"),
  matched: state("Matched to installment", 1, 0, "terminal"),
  unmatched: state("Unmatched", 1, 1, "review"),
  aml: state("AML review", 2, 0, "review"),
  reclassified: state("Reclassified to next week", 2, 1, "terminal"),
  exception: state("Exception queue", 2, 2, "review"),
  cleared: state("Cleared", 3, 0, "terminal"),
  blocked: state("Blocked", 3, 1, "refused"),
}, [
  step("received", "matched", "match", "LMS installment matched within T+2", repaymentSystems, "Payment settlement and the installment must agree."),
  step("received", "unmatched", "flagUnmatched", "No timely installment match", repaymentSystems, "Compare the bank's settled reference to the LMS allocation."),
  step("unmatched", "aml", "screenPayer", "Third party · monthly amount > EGP 50K", ["fawry", "salesforce", "postgres"], "Resolve the Fawry payer against the borrower joined from CRM and the LMS."),
  step("aml", "cleared", "clear", "Payer and source approved", ["fawry", "salesforce"], "Link the compliance decision in CRM to the payment evidence."),
  step("aml", "blocked", "block", "Screening refused", ["fawry", "salesforce"], "Leave the settlement blocked when the CRM review refuses the payer."),
  step("unmatched", "reclassified", "reclassify", "Late or misdated · bank confirms next week", repaymentSystems, "Use the bank value date and preserve the earlier reconciliation reading.", "Payment.reclassify"),
  step("unmatched", "exception", "openException", "No allocation after T+2", repaymentSystems, "Hold the bank settlement for review until the LMS allocation is resolved."),
  step("exception", "matched", "resolveException", "Installment confirmed", repaymentSystems, "Close the exception only when the bank reference matches the LMS installment."),
])

export const MACHINES = [
  { id: "origination", type: "loan", property: "Loan.status", title: "Loan lifecycle", phases: [origination, servicing] },
  { id: "brokerage", type: "brokerage-account", property: "BrokerageAccount.status", title: "Brokerage onboarding", phases: [onboarding] },
  { id: "settlement", type: "payment", property: "Payment.status", title: "Settlement handling", phases: [settlements] },
] as const

export type FinanceMachine = typeof MACHINES[number]

export const PROCESS_MANAGERS = [
  { id: "collections", title: "Collections process", description: "Coordinates events from Loan servicing with the Client's CRM case and the repayment schedule.", branches: [
    { event: "InstallmentMissed", aggregate: "loan", reads: "Hardship case on the Client", when: "Open hardship case", command: "Installment.pauseDunning", rule: "hardship", systems: ["postgres", "salesforce"], target: "loan" },
    { event: "InstallmentMissed", aggregate: "loan", reads: "Hardship case on the Client", when: "No open hardship case", command: "PromiseToPay.request", rule: "hardship", systems: ["postgres", "salesforce"], target: "loan" },
    { event: "HardshipOpened", aggregate: "client", reads: "Active loan and revised schedule", when: "Schedule approved", command: "Loan.restructure", rule: "hardship", systems: ["salesforce", "postgres"], target: "loan" },
  ] },
  { id: "brokerage-hold", title: "Brokerage hold", description: "Uses arrears on a Loan to guard actions on a Brokerage account for the same Client.", branches: [
    { event: "LoanArrearsChanged", aggregate: "loan", reads: "Joined Loan and Brokerage account by Client", when: "Loan enters arrears or changes DPD · DPD ≥ 30", command: "BrokerageAccount.blockMargin", rule: "margin", systems: ["postgres", "egx", "mcdr"], target: "brokerage-account" },
  ] },
] as const satisfies readonly {
  id: string; title: string; description: string; branches: readonly {
    event: FinanceEvent; aggregate: FinanceType; reads: string; when: string; command: FinanceCommand
    rule: RuleId; systems: StateStep["systems"]; target: FinanceType
  }[]
}[]
