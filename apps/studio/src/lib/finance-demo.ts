/** The preview build may present its recorded examples as synthetic finance data. */
export const financeDemo = import.meta.env?.VITE_STUDIO_FINANCE_DEMO === "1"

const labels: readonly (readonly [RegExp, string])[] = [
  [/Owner and location partitioned holding/gi, "Repayment by client and branch"],
  [/Order-line commitment/gi, "Approved loan commitment"],
  [/Delivery observation/gi, "Disbursement observation"],
  [/Source party role/gi, "Client relationship"],
  [/Signed ledger observation, not a posting command/gi, "Recorded ledger entry"],
  [/Gold scrap/gi, "Loan repayment"],
  [/Recovered mass exceeds source input/gi, "Reconciled amount exceeds expected payment"],
  [/canon\.PartyRole/g, "canon.ClientRole"],
  [/canon\.Party\b/g, "canon.Client"],
  [/canon\.Holding\b/g, "canon.Repayment"],
  [/canon\.OrderLine\b/g, "canon.Loan"],
  [/canon\.Delivery\b/g, "canon.Disbursement"],
  [/canon\.Item\b/g, "canon.LoanProduct"],
  [/canon\.StockValuation\b/g, "canon.LoanValuation"],
  [/demo\.Conversion\b/g, "demo.Repayment"],
  [/demo\.yield_week\b/g, "demo.repayments_reconciled_week"],
  [/demo\.yield\b/g, "demo.repayments_reconciled"],
  [/demo\.input\b/g, "demo.expected_payments"],
  [/demo\.recovered\b/g, "demo.reconciled_payments"],
  [/"Yield"/g, '"Repayments reconciled"'],
  [/"Input"/g, '"Expected payments"'],
  [/"Recovered"/g, '"Reconciled payments"'],
  [/SYN-MELT/g, "SYN-PAYMENT"],
  [/SYN-SHOPIFY-holding/g, "SYN-SHOPIFY-payment"],
  [/SYN-ERPNEXT-order/g, "SYN-ERPNEXT-loan"],
  [/demo([/~])holding\1owner\1(north|south)\1gold/g, "demo$1repayment$1client$1$2$1payment"],
  [/demo([/~])order\1(\d+)\1line\1(\d+)/g, "demo$1loan$1$2$1entry$1$3"],
  [/demo([/~])item\1gold/g, "demo$1product$1personal-loan"],
  [/demo([/~])party/g, "demo$1client"],
  [/constitution\.melt/g, "constitution.repayment"],
  [/constitution\.fine_mass/g, "constitution.reconciliation_amount"],
  [/input_fine_g/g, "expected_amount"],
  [/output_fine_g/g, "reconciled_amount"],
  [/loss_fine_g/g, "unreconciled_amount"],
  [/InventoryLevel/g, "PaymentRecord"],
  [/Sales Order Item/g, "Loan Entry"],
  [/Delivery Note Item/g, "Disbursement Entry"],
  [/holding(?=[._])/g, "repayment"],
  [/quantity_basis/g, "amount_basis"],
  [/stock_cost/g, "ledger_amount"],
  [/stock valuation/gi, "loan valuation"],
  [/Valued stock/g, "Valued loans"],
  [/never stock/g, "separate from balances"],
  [/resource_code/g, "product_code"],
  [/"mass"/g, '"amount"'],
  [/"metal"/g, '"product"'],
  [/"system":\s*"ucum"(\s*,\s*"code":\s*)"g"/g, '"system":"iso4217"$1"EGP"'],
  [/"g"/g, '"EGP"'],
  [/\bUSD\b/g, "EGP"],
  [/GOLD/g, "PERSONAL_LOAN"],
  [/\bgold\b/gi, "personal loan"],
  [/\bmelt\b/gi, "repayment"],
]

/** Relabel the sample and its record paths together, preserving the original numeric text and links. */
export function financeRecording(text: string): string {
  return labels.reduce((result, [pattern, replacement]) => result.replace(pattern, replacement), text)
}

export const financeLifecycleTitle = (name: string): string =>
  financeDemo && name === "SalesOrder" ? "Loan application" : name
