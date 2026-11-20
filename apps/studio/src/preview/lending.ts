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
  manager: "Omar Fouad",
  phone: "+20 10 •••• 0142",
} as const

/** One fact on the borrower's record, with the system it came from. */
export interface Fact { readonly label: string; readonly value: string; readonly from: SystemId; readonly alert?: boolean; readonly note?: string }

/** The full record the object view shows, after the freeze. */
export const RECORD: readonly Fact[] = [
  { label: "Loan", value: `${"LN-2041"} · Personal loan`, from: "fineract" },
  { label: "Outstanding", value: "EGP 186,400", from: "fineract" },
  { label: "Installment", value: "EGP 7,250 · due on the 15th", from: "fineract" },
  { label: "Missed payments", value: "2 in 60 days", from: "payments", alert: true },
  { label: "Last payment", value: "15 Sep 2026", from: "payments" },
  { label: "Credit limit", value: "EGP 60,000 · Frozen", from: "fineract", alert: true },
  { label: "Credit score", value: "612", from: "bureau", note: "was 688" },
  { label: "Phone", value: "+20 10 •••• 0142", from: "crm" },
  { label: "Relationship manager", value: "Omar Fouad", from: "crm" },
]

/** What each system sends into the record. */
export const FEEDS: Readonly<Record<SystemId, string>> = {
  fineract: "Loan, balance, schedule",
  payments: "Installments, transfers",
  crm: "Contact, relationship",
  bureau: "Credit score, history",
}

/** The record on 1 Sep and today, fact by fact. */
export const THEN_NOW: readonly { readonly label: string; readonly then: string; readonly now: string; readonly from: SystemId }[] = [
  { label: "Outstanding", then: "EGP 191,650", now: "EGP 186,400", from: "fineract" },
  { label: "Missed payments", then: "0", now: "2 in 60 days", from: "payments" },
  { label: "Credit score", then: "688", now: "612", from: "bureau" },
  { label: "Credit limit", then: "EGP 60,000 · Open", now: "EGP 60,000 · Frozen", from: "fineract" },
  { label: "Phone", then: "+20 12 •••• 7710", now: "+20 10 •••• 0142", from: "crm" },
]

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

/** Someone the action refuses: Sales may see the record, not act on it. */
export const BLOCKED = { name: "Karim Fathy", role: "Sales", initials: "KF" } as const

export const ACTION = {
  title: "Freeze limit + call",
  detail: `Freeze the ${BORROWER.creditLine} credit limit and book a call`,
  when: `${TODAY}, 14:05`,
  why: "2 missed payments in 60 days",
} as const

/** The rules this record keeps, in the words they were written in. */
/** The business rule the freeze is held to, as a check line. */
export const RULE_CHECK = "Rule: loan over EGP 50,000 with 2 missed in 60 days"

export const RULES: readonly string[] = [
  "Only Collections or Risk can freeze a credit limit.",
  "A loan over EGP 50,000 with 2 missed payments in 60 days is flagged.",
  "Phone numbers are masked for anyone outside Collections.",
]

/** What changed on the record, newest first: who or what, when, and why. */
export interface Change { readonly when: string; readonly who: string; readonly from?: SystemId; readonly what: string; readonly why: string; readonly tone?: Tone }

export const CHANGES: readonly Change[] = [
  { when: "20 Nov, 14:05", who: ACTOR.name, what: "Froze the credit limit and booked a call", why: ACTION.why, tone: "red" },
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
