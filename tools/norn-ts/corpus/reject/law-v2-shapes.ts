// Law predicates are inline lambdas in the lambda subset (dx-spec §4.5); a
// function value, mutation or an ambient read is refused at the author's line.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Account = Norn.type({
  name: "books.Account",
  title: "Account",
  version: "0.1.0",
  properties: Schema.Struct({ account_id: Schema.String, balance: Schema.Int }),
  identity: [{ name: "primary", properties: ["account_id"] }],
})

const solvent = (self: { readonly balance: number }) => self.balance >= 0

export const byValue = Norn.law("by_value", { on: Account, must: solvent, because: "x" })
export const mutable = Norn.law("mutable", {
  on: Account,
  must: (self) => {
    let floor = 0
    return self.balance >= floor
  },
  because: "x",
})
export const ambient = Norn.law("ambient", { on: Account, must: (self) => self.balance < Date.now(), because: "x" })
