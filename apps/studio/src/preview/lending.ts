/**
 * The finale pitch's sample world: one synthetic lender, one synthetic borrower. Every preview
 * screen reads its numbers from here, so a figure is the same on every slide. Nothing here is a
 * real person, bank or prospect, and nothing here is read from or written to a cell.
 */
import type { Tone } from "@/components/chip"

export type SystemId = "fineract" | "payments" | "crm" | "bureau"

/** The four systems the borrower's record is woven from, each with its chip tone. */
export const SYSTEMS: Readonly<Record<SystemId, { readonly name: string; readonly role: string; readonly tone: Tone }>> = {
  fineract: { name: "Fineract", role: "Core lending", tone: "blue" },
  payments: { name: "Payments", role: "Collections and transfers", tone: "teal" },
  crm: { name: "CRM", role: "Customer contact", tone: "violet" },
  bureau: { name: "Bureau", role: "Credit bureau", tone: "amber" },
}

export const TODAY = "20 Nov 2026"

export const BORROWER = {
  name: "Mona Adel",
  initials: "MA",
  detail: "Personal loan · Cairo",
  loan: "LN-2041",
  outstanding: "EGP 186,400",
  installment: "EGP 7,250",
  creditLine: "EGP 60,000",
  score: "612",
  scoreBefore: "688",
  manager: "Karim Fouad",
  phone: "+20 10 •••• 0142",
} as const

/** One fact on the borrower's record, with the system it came from. */
export interface Fact { readonly label: string; readonly value: string; readonly from: SystemId; readonly alert?: boolean; readonly note?: string }

export const FACTS: readonly Fact[] = [
  { label: "Loan", value: BORROWER.loan, from: "fineract" },
  { label: "Outstanding", value: BORROWER.outstanding, from: "fineract" },
  { label: "Missed payments", value: "2 in 60 days", from: "payments", alert: true },
  { label: "Credit score", value: BORROWER.score, from: "bureau", note: `was ${BORROWER.scoreBefore}` },
  { label: "Phone", value: BORROWER.phone, from: "crm" },
]

/** Monthly installments, due on the 15th: five paid on time, then two missed. */
export const PAYMENTS: readonly { readonly month: string; readonly paid: boolean }[] = [
  { month: "May", paid: true }, { month: "Jun", paid: true }, { month: "Jul", paid: true }, { month: "Aug", paid: true },
  { month: "Sep", paid: true }, { month: "Oct", paid: false }, { month: "Nov", paid: false },
]

export type Can = "see" | "edit" | "act"

/** Who may do what on this record: the server's policy, shown, never decided here. */
export const ROLES: readonly { readonly role: string; readonly can: readonly Can[]; readonly note?: string }[] = [
  { role: "Collections", can: ["see", "edit", "act"] },
  { role: "Risk", can: ["see", "act"] },
  { role: "Sales", can: ["see"] },
  { role: "Data team", can: ["see"], note: "phone masked" },
]

export const ACTOR = { name: "Sara Nabil", role: "Collections lead", initials: "SN" } as const

export const ACTION = {
  title: "Freeze limit + call",
  detail: `Freeze the ${BORROWER.creditLine} credit line and book a call`,
  when: `${TODAY}, 14:05`,
  why: "2 missed payments in 60 days",
} as const

/** The rules this record keeps, in the words they were written in. */
export const RULES: readonly string[] = [
  "Only Collections or Risk can freeze a credit line.",
  "A loan over EGP 50,000 with 2 missed payments in 60 days is flagged.",
  "Phone numbers are masked for anyone outside Collections.",
]

/** What changed on the record, newest first: who or what, when, and why. */
export interface Change { readonly when: string; readonly who: string; readonly from?: SystemId; readonly what: string; readonly why: string; readonly tone?: Tone }

export const CHANGES: readonly Change[] = [
  { when: "20 Nov, 14:05", who: ACTOR.name, what: "Froze the credit line and booked a call", why: ACTION.why, tone: "red" },
  { when: "18 Nov, 09:12", who: "Bureau sync", from: "bureau", what: `Credit score ${BORROWER.scoreBefore} → ${BORROWER.score}`, why: "Monthly bureau refresh" },
  { when: "15 Nov, 23:59", who: "Payments", from: "payments", what: `Installment of ${BORROWER.installment} missed`, why: "No payment by the due date", tone: "red" },
  { when: "3 Nov, 11:40", who: BORROWER.manager, from: "crm", what: "Phone number updated", why: "Customer called the branch" },
  { when: "15 Oct, 23:59", who: "Payments", from: "payments", what: `Installment of ${BORROWER.installment} missed`, why: "No payment by the due date", tone: "red" },
  { when: "15 Sep, 10:22", who: "Payments", from: "payments", what: `Installment of ${BORROWER.installment} paid`, why: "Paid on time", tone: "green" },
]

/** The book under the live rule and under the forked one. */
export const BOOK = {
  activeLoans: "12,480",
  live: { threshold: 2, loans: 37, exposure: "EGP 6,214,500", calls: 37, mona: true },
  fork: { threshold: 3, loans: 14, exposure: "EGP 2,486,300", calls: 14, mona: false },
  difference: { loans: "−23", exposure: "−EGP 3,728,200" },
} as const
