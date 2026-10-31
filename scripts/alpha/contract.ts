/** Alpha acceptance #3762. Adapters call the real engine; no fixture responders. */
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type Surface = "api" | "mcp" | "cli";
export type Actor = "operator" | "forbidden";
export interface Tenant { orgId: string; environmentId: string }
export type Seam<T> = { kind: "pending"; reason: string } | { kind: "ready"; adapter: T };
export interface Installation {
  version: string;
  validation: "valid";
  plan: "additive" | "breaking";
}
export interface Definitions {
  compileDefinition(source: string): Promise<Json>;
  installDefinition(tenant: Tenant, compiledDefinition: Json): Promise<Installation>;
  /** Run the real lifecycle/refinement checker; failure rejects the promise. */
  checkDefinition(compiledDefinition: Json): Promise<void>;
}
export type Invocation = { kind: "accepted" } | { kind: "refused"; code: string };
export interface Query { tenant: Tenant; orderId: string }
export interface OrderState {
  status: "draft" | "submitted" | "approved" | "cancelled";
  invoice_count: number;
  /** Exact decimal wire values, not JS floating-point sums. */
  invoiced_total: string;
  allocated_total: string;
  invoiced: boolean;
  completed: boolean;
  outstanding: string;
}
export interface Subscription {
  /** Resolve the next state, or reject on timeout/disconnect. */
  next(): Promise<OrderState>;
  close(): Promise<void>;
}
export interface Surfaces {
  /** Bound to Runtime's fresh tenant. Map operator per action: sales for
   * create/submit/cancel, approver for approve, billing for record_invoice,
   * treasury for record_payment. Never use an all-powerful shared principal. */
  invoke(surface: Surface, action: string, args: { [key: string]: Json }, actor: Actor): Promise<Invocation>;
  subscribe(query: Query): Promise<Subscription>;
  query(query: Query): Promise<OrderState>;
}
export interface LedgerSnapshot {
  /** Verified, ordered ledger event identities; include definition installs. */
  events: string[];
  state: OrderState;
}
export interface Runtime {
  /** Create a disposable, empty tenant; close removes it even on failure. */
  open(): Promise<Tenant>;
  close(): Promise<void>;
  snapshot(query: Query): Promise<LedgerSnapshot>;
  /** Replay the complete retained ledger into a fresh store, not a cached view. */
  replay(query: Query): Promise<LedgerSnapshot>;
}
