import { DurableObject } from "cloudflare:workers"
import { EMPTY, LIMITS, admit, spend, type Admission, type Ledger, type Scope } from "./limits.ts"

/** One spending scope's ledger: a session, a visitor or the whole Worker. */
export class AgentBudget extends DurableObject {
  private async ledger(): Promise<Ledger> {
    return (await this.ctx.storage.get<Ledger>("ledger")) ?? EMPTY
  }

  async admit(scope: Scope): Promise<Admission> {
    const admission = admit(await this.ledger(), LIMITS[scope], Date.now())
    await this.ctx.storage.put("ledger", admission.ledger)
    return admission
  }

  async spend(tokens: number): Promise<void> {
    await this.ctx.storage.put("ledger", spend(await this.ledger(), tokens, Date.now()))
  }
}
