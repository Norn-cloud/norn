import type { Tone } from "@/components/chip"

export type MarkId = "postgres" | "iscore" | "salesforce" | "fawry" | "instapay" | "egx" | "mcdr" | "valify" | "fra" | "aws" | "leasing" | "ledger" | "norn" | "model"

export const MARKS: Readonly<Record<MarkId, { name: string; tone: Tone; file?: string; letters?: string }>> = {
  postgres: { name: "PostgreSQL", tone: "blue", file: "postgresql.org.png" },
  iscore: { name: "I-Score", tone: "amber", letters: "IS" },
  salesforce: { name: "Salesforce", tone: "violet", file: "salesforce.com.png" },
  fawry: { name: "Fawry", tone: "teal", file: "fawry.com.png" },
  instapay: { name: "InstaPay", tone: "green", letters: "IP" },
  egx: { name: "EGX", tone: "orange", letters: "EGX" },
  mcdr: { name: "MCDR", tone: "teal", letters: "MCDR" },
  valify: { name: "Valify", tone: "pink", file: "valify.me.png" },
  fra: { name: "FRA", tone: "green", file: "fra.gov.eg.png" },
  leasing: { name: "Leasing", tone: "teal", letters: "LE" },
  ledger: { name: "General ledger", tone: "violet", letters: "GL" },
  model: { name: "MLflow", tone: "violet", letters: "ML" },
  norn: { name: "Declared in Norn", tone: "blue", letters: "N" },
  aws: { name: "AWS", tone: "amber", file: "aws.amazon.com.png" },
}

export interface MappedSystem {
  readonly id: string
  readonly name: string
  readonly marks: readonly MarkId[]
  readonly schema: string
  readonly datasets: readonly (readonly [name: string, rows: number])[]
  readonly syncedAt: string
}

export const SYSTEMS: readonly MappedSystem[] = [
  { id: "lms", name: "Core lending (LMS)", marks: ["postgres"], schema: "lms.public.v3", datasets: [["loans", 12_480], ["installments", 94_620], ["disbursements", 11_930]], syncedAt: "09:42" },
  { id: "credit", name: "I-Score credit bureau", marks: ["iscore"], schema: "iscore.credit.v2", datasets: [["credit reports", 8_614], ["scores", 25_842], ["inquiries", 10_406]], syncedAt: "09:36" },
  { id: "crm", name: "Salesforce CRM", marks: ["salesforce"], schema: "crm.customer.v2", datasets: [["clients", 9_208], ["relationship managers", 34], ["cases", 1_427]], syncedAt: "09:40" },
  { id: "payments", name: "Bank settlement", marks: ["fawry", "instapay"], schema: "payments.settlement.v4", datasets: [["collections", 71_560], ["settlements", 18_286]], syncedAt: "09:42" },
  { id: "brokerage", name: "Brokerage", marks: ["egx", "mcdr"], schema: "brokerage.trades.v2", datasets: [["accounts", 3_126], ["orders", 26_940], ["holdings", 7_804]], syncedAt: "09:31" },
  { id: "kyc", name: "e-KYC", marks: ["valify"], schema: "valify.checks.v1", datasets: [["identity checks", 9_234]], syncedAt: "09:38" },
  { id: "reporting", name: "FRA regulatory reporting", marks: ["fra"], schema: "fra.monthly.v1", datasets: [["monthly returns", 24]], syncedAt: "09:20" },
]

export const TYPES = {
  client: { name: "Client", plural: "Clients", mark: "salesforce", tone: "blue" },
  loan: { name: "Loan", plural: "Loans", mark: "postgres", tone: "teal" },
  installment: { name: "Installment", plural: "Installments", mark: "postgres", tone: "violet" },
  payment: { name: "Payment", plural: "Payments", mark: "fawry", tone: "green" },
  "credit-report": { name: "Credit report", plural: "Credit reports", mark: "iscore", tone: "amber" },
  "kyc-check": { name: "KYC check", plural: "KYC checks", mark: "valify", tone: "pink" },
  "relationship-manager": { name: "Relationship manager", plural: "Relationship managers", mark: "salesforce", tone: "violet" },
  "hardship-case": { name: "Hardship case", plural: "Hardship cases", mark: "salesforce", tone: "pink" },
  "brokerage-account": { name: "Brokerage account", plural: "Brokerage accounts", mark: "mcdr", tone: "teal" },
  order: { name: "Order", plural: "Orders", mark: "egx", tone: "orange" },
  "regulatory-return": { name: "Regulatory return", plural: "Regulatory returns", mark: "fra", tone: "green" },
} as const satisfies Readonly<Record<string, { name: string; plural: string; mark: MarkId; tone: Tone }>>

export type FinanceType = keyof typeof TYPES

export const CLIENT = {
  id: "CL-1042", name: "Mariam Nasser", nationalId: "DEMO-NID-1042", payerRef: "PAY-1042",
  account: "BA-1042", brokerOrder: "ORD-1042", phone: "+20 1X XXX X0017", address: "12 Sample Avenue, Cairo",
  birth: "14 Feb 1987", regularPayment: 24_750, document: "STMT-1042", report: "CR-1042",
  depositAmount: 125_000, depositRef: "DEP-1042", depositDate: "20 Nov 2026",
} as const

export const CLIENT_FIELDS = [
  { mark: "postgres", field: "client_id" },
  { mark: "fawry", field: "payer_ref" },
  { mark: "iscore", field: "national_id" },
  { mark: "salesforce", field: "cust_nm" },
] as const

export const ROW_COUNT = new Intl.NumberFormat("en-GB")

type Cardinality = "1" | "0..1" | "0..*"
type ModelLink = readonly [from: FinanceType, forward: string, to: FinanceType, inverse: string, outgoing: Cardinality, incoming: Cardinality]

export const MODEL_LINKS: readonly ModelLink[] = [
  ["client", "holds", "loan", "is held by", "0..*", "1"],
  ["loan", "has", "installment", "belongs to", "0..*", "1"],
  ["installment", "is paid by", "payment", "pays", "0..*", "0..1"],
  ["client", "reported by", "credit-report", "reports on", "0..*", "1"],
  ["client", "verified by", "kyc-check", "verifies", "0..*", "1"],
  ["relationship-manager", "manages", "client", "is managed by", "0..*", "0..1"],
  ["client", "has", "hardship-case", "supports", "0..*", "1"],
  ["client", "holds", "brokerage-account", "is held by", "0..*", "1"],
  ["brokerage-account", "has", "order", "is placed on", "0..*", "1"],
  ["loan", "filed in", "regulatory-return", "includes", "0..*", "0..*"],
]
