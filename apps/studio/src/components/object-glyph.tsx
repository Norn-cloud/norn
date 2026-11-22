import { ReceiptText, ArrowLeftRight, Box, ClipboardList, FileCheck2, Layers, Link2, Package, Receipt, Scale, Truck, UserRound, Users, Wallet, Warehouse, type LucideIcon } from "lucide-react"
import { IconTile, type Tone } from "@/components/chip"

type Appearance = { readonly icon: LucideIcon; readonly tone: Tone }

const OBJECTS: Readonly<Record<string, Appearance>> = {
  "canon.Repayment": { icon: ReceiptText, tone: "green" },
  "canon.Loan": { icon: ClipboardList, tone: "orange" },
  "canon.Client": { icon: UserRound, tone: "blue" },
  "canon.Holding": { icon: Warehouse, tone: "blue" },
  "canon.OrderLine": { icon: ClipboardList, tone: "violet" },
  "canon.Delivery": { icon: Truck, tone: "teal" },
  "canon.Item": { icon: Package, tone: "amber" },
  "canon.Party": { icon: UserRound, tone: "pink" },
  "canon.PartyRole": { icon: Users, tone: "pink" },
  "canon.Posting": { icon: Receipt, tone: "green" },
  "canon.SettlementObservation": { icon: ArrowLeftRight, tone: "teal" },
  "canon.ValuationPolicy": { icon: Scale, tone: "orange" },
  "core.analytics.Restatement": { icon: ArrowLeftRight, tone: "violet" },
  "norn.ObservedLinkClaim": { icon: Link2, tone: "blue" },
  Client: { icon: UserRound, tone: "blue" },
  Account: { icon: Wallet, tone: "violet" },
  Transaction: { icon: ArrowLeftRight, tone: "teal" },
  KYC: { icon: FileCheck2, tone: "pink" },
  Posting: { icon: Receipt, tone: "green" },
  RiskProfile: { icon: Scale, tone: "amber" },
  Loan: { icon: ClipboardList, tone: "orange" },
  Repayment: { icon: Receipt, tone: "green" },
}

export function objectAppearance(name: string, shared = false): Appearance {
  if (Object.hasOwn(OBJECTS, name)) return OBJECTS[name]!
  return shared ? { icon: Layers, tone: "gray" } : { icon: Box, tone: "blue" }
}

export function ObjectTile({ name, shared, className }: { readonly name: string; readonly shared?: boolean; readonly className?: string }) {
  const { icon: Icon, tone } = objectAppearance(name, shared)
  return <IconTile tone={tone} className={className}><Icon /></IconTile>
}
