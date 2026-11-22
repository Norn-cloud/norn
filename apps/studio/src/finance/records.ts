import { CLIENT, ROW_COUNT, TYPES, type FinanceType, type MarkId } from "./data.ts"

export const egp = (amount: number) => `EGP ${ROW_COUNT.format(amount)}`
export const FINANCE_CUTOFF = "22 Nov 2026, 09:36 EET"

export interface Fact {
  readonly label: string
  readonly value: string
  readonly source: MarkId
  readonly path: string
  readonly mapping: string
  readonly target: string
  readonly at: string
}

export interface FinanceRecord {
  readonly id: string
  readonly type: FinanceType
  readonly name: string
  readonly status: string
  readonly summary: string
  readonly facts: readonly Fact[]
  readonly history?: readonly { readonly date: string; readonly score: number }[]
}

export interface FinanceLink {
  readonly from: string
  readonly to: string
  readonly label: "holds" | "is paid by" | "manages" | "reported by" | "has" | "verified by" | "filed in"
}

const fact = (label: string, value: string, source: MarkId, path: string, target: string, mapping = "Finance field map"): Fact =>
  ({ label, value, source, path, target, mapping, at: FINANCE_CUTOFF })

const PEOPLE = [
  { ref: "1042", name: CLIENT.name, net: 1_250_000, due: CLIENT.regularPayment, score: 724, relationshipManager: "Nour Hassan", city: "Cairo" },
  { ref: "1087", name: "Omar Saleh", net: 585_000, due: 14_650, score: 692, relationshipManager: "Yara Kamal", city: "Giza" },
  { ref: "1163", name: "Salma Farid", net: 2_400_000, due: 63_800, score: 768, relationshipManager: "Kareem Adel", city: "Alexandria" },
  { ref: "1246", name: "Hassan Amin", net: 180_000, due: 6_240, score: 645, relationshipManager: "Rana Osman", city: "Cairo" },
  { ref: "1325", name: "Nadia Sobhy", net: 4_200_000, due: 107_500, score: 784, relationshipManager: "Ahmed Samir", city: "Giza" },
  { ref: "1418", name: "Youssef Saad", net: 98_000, due: 3_890, score: 670, relationshipManager: "Dina Helmy", city: "Cairo" },
] as const

function loanSet(ref: string, suffix: "A" | "B", client: string, amount: number, due: number): readonly FinanceRecord[] {
  const key = `${ref}-${suffix}`
  const loan = `LN-${key}`
  const installment = `INS-${key}`
  const payment = `FWR-${key}`
  return [
    { id: loan, type: "loan", name: suffix === "A" ? "Term loan" : "Personal loan", status: "Active", summary: egp(amount), facts: [
      fact("Principal", egp(amount), "postgres", "lms.loans.principal_egp", "Loan.principal"),
      fact("Client", client, "postgres", "lms.loans.client_id", "Loan.clientId"),
      fact("Monthly installment", egp(due), "postgres", "lms.loans.installment_egp", "Loan.installmentAmount"),
      fact("Disbursement date", "19 Oct 2026", "postgres", "lms.disbursements.posted_on", "Loan.disbursedOn"),
    ] },
    { id: installment, type: "installment", name: "November installment", status: "Paid", summary: egp(due), facts: [
      fact("Installment due", egp(due), "postgres", "lms.installments.due_egp", "Installment.amount"),
      fact("Due date", "19 Nov 2026", "postgres", "lms.installments.due_on", "Installment.dueDate"),
      fact("Loan", loan, "postgres", "lms.installments.loan_id", "Installment.loanId"),
    ] },
    { id: payment, type: "payment", name: "Fawry repayment", status: "Settled", summary: egp(due), facts: [
      fact("Payment amount", egp(due), "fawry", "$.settlement.amount_egp", "Payment.amount", "Fawry cash collection"),
      fact("Currency", "EGP", "fawry", "$.settlement.currency", "Payment.currency"),
      fact("Payer reference", `PAY-${ref}`, "fawry", "$.collection.payer_ref", "Payment.payerRef"),
      fact("Settlement date", "20 Nov 2026", "fawry", "$.settlement.posted_on", "Payment.settledOn"),
    ] },
  ]
}

export const RECORDS: readonly FinanceRecord[] = [
  ...PEOPLE.flatMap(({ ref, name, net, due, score, relationshipManager, city }): FinanceRecord[] => [
    { id: `CL-${ref}`, type: "client", name, status: "Active", summary: city, facts: [
      fact("Client name", name, "salesforce", "Client__c.cust_nm", "Client.name", "Client identity join"),
      fact("LMS client ID", `LMS-${ref}`, "postgres", "lms.loans.client_id", "Client.lmsClientId", "Client identity join"),
      fact("National ID", `DEMO-NID-${ref}`, "iscore", "$.subject.national_id", "Client.nationalId", "Client identity join"),
      fact("Payer reference", `PAY-${ref}`, "fawry", "$.collection.payer_ref", "Client.payerRef", "Client identity join"),
    ] },
    ...loanSet(ref, "A", name, net, due),
    { id: `CR-${ref}`, type: "credit-report", name: "I-Score bureau report", status: "Current", summary: `Score ${score}`, facts: [
      fact("Risk score", String(score), "iscore", "$.subject.score", "CreditReport.score", "Bureau score mapping"),
      fact("Credit band", score >= 700 ? "Low risk" : "Moderate risk", "iscore", "$.subject.risk_band", "CreditReport.riskBand", "Bureau score mapping"),
      fact("Report date", "22 Nov 2026", "iscore", "$.report.issued_on", "CreditReport.issuedOn"),
    ], history: [{ date: "22 Sep", score: score - 36 }, { date: "22 Oct", score: score - 18 }, { date: "22 Nov", score }] },
    { id: `KYC-${ref}`, type: "kyc-check", name: "Identity verification", status: "Verified", summary: "Document + liveness", facts: [
      fact("Verification result", "Verified", "valify", "$.checks.identity.status", "KycCheck.status"),
      fact("Verified on", ref === "1042" ? "5 Nov 2026" : "18 Oct 2026", "valify", "$.checks.identity.verified_on", "KycCheck.verifiedOn"),
      ...(ref === "1042" ? [fact("Previous verification expired", "3 Nov 2026", "valify", "$.checks.previous.expires_on", "KycCheck.previousExpiresOn")] : []),
      fact("Client", name, "salesforce", "Client__c.cust_nm", "KycCheck.client"),
    ] },
    { id: `RM-${ref}`, type: "relationship-manager", name: relationshipManager, status: "Assigned", summary: `${city} relationship desk`, facts: [
      fact("Manager name", relationshipManager, "salesforce", "User.Name", "RelationshipManager.name"),
      fact("Assigned client", name, "salesforce", "Client__c.cust_nm", "RelationshipManager.client"),
    ] },
    { id: `HC-${ref}`, type: "hardship-case", name: "Payment schedule review", status: ref === "1042" ? "In review" : "Resolved", summary: "Repayment assistance", facts: [
      fact("Case category", "Repayment assistance", "salesforce", "Case.Reason", "HardshipCase.category"),
      fact("Opened on", "16 Nov 2026", "salesforce", "Case.CreatedDate", "HardshipCase.openedOn"),
      fact("Client", name, "salesforce", "Client__c.cust_nm", "HardshipCase.client"),
    ] },
    { id: `BA-${ref}`, type: "brokerage-account", name: "Brokerage account", status: "Open", summary: egp(735_000 + (Number(ref) - 1042) * 100), facts: [
      fact("Account number", `BA-${ref}`, "mcdr", "$.account.account_no", "BrokerageAccount.number"),
      fact("Account balance", egp(735_000 + (Number(ref) - 1042) * 100), "mcdr", "$.account.balance_egp", "BrokerageAccount.balance"),
      fact("Account holder", name, "salesforce", "Client__c.cust_nm", "BrokerageAccount.client"),
    ] },
    { id: `ORD-${ref}`, type: "order", name: "Executed buy order", status: "Executed", summary: egp(82_500 + (Number(ref) - 1042) * 20), facts: [
      fact("Order value", egp(82_500 + (Number(ref) - 1042) * 20), "egx", "$.order.value_egp", "Order.value"),
      fact("Brokerage account", `BA-${ref}`, "egx", "$.order.account_no", "Order.accountNumber"),
    ] },
  ]),
  ...loanSet("1042", "B", CLIENT.name, 375_000, 13_500),
  { id: "FWR-SET-1027", type: "payment", name: "Fawry settlement batch", status: "Reclassified", summary: egp(150_000), facts: [
    fact("Settlement amount", egp(150_000), "fawry", "$.settlement.batch_amount_egp", "Payment.amount", "Fawry cash collection"),
    fact("Bank value date", "16 Nov 2026", "fawry", "$.settlement.value_date", "Payment.settledOn"),
    fact("Previously reported in", "Week of 9 Nov 2026", "fawry", "$.settlement.reporting_week", "Payment.reportingWeek"),
  ] },
  ...["Jun", "Jul", "Aug", "Sep", "Oct", "Nov"].map((month, i): FinanceRecord => ({
    id: `FRA-2026-${String(i + 6).padStart(2, "0")}`, type: "regulatory-return", name: `${month} 2026 lending return`,
    status: i === 5 ? "Draft" : "Filed", summary: `${month} reporting period`, facts: [
      fact("Reporting period", `${month} 2026`, "fra", "$.return.reporting_period", "RegulatoryReturn.period"),
      fact("Portfolio", "Consumer lending", "fra", "$.return.portfolio", "RegulatoryReturn.portfolio"),
    ],
  })),
]

export const LINKS: readonly FinanceLink[] = [
  ...PEOPLE.flatMap(({ ref }, i): FinanceLink[] => [
    { from: `CL-${ref}`, to: `LN-${ref}-A`, label: "holds" },
    { from: `LN-${ref}-A`, to: `INS-${ref}-A`, label: "has" },
    { from: `INS-${ref}-A`, to: `FWR-${ref}-A`, label: "is paid by" },
    { from: `CL-${ref}`, to: `CR-${ref}`, label: "reported by" },
    { from: `CL-${ref}`, to: `KYC-${ref}`, label: "verified by" },
    { from: `RM-${ref}`, to: `CL-${ref}`, label: "manages" },
    { from: `CL-${ref}`, to: `HC-${ref}`, label: "has" },
    { from: `CL-${ref}`, to: `BA-${ref}`, label: "holds" },
    { from: `BA-${ref}`, to: `ORD-${ref}`, label: "has" },
    { from: `LN-${ref}-A`, to: `FRA-2026-${String(i + 6).padStart(2, "0")}`, label: "filed in" },
  ]),
  { from: CLIENT.id, to: "LN-1042-B", label: "holds" },
  { from: "LN-1042-B", to: "INS-1042-B", label: "has" },
  { from: "INS-1042-B", to: "FWR-1042-B", label: "is paid by" },
  { from: "LN-1042-B", to: "FRA-2026-10", label: "filed in" },
]

export const BY_ID = new Map(RECORDS.map((record) => [record.id, record]))

export function isFinanceType(value: unknown): value is FinanceType {
  return typeof value === "string" && Object.hasOwn(TYPES, value)
}
