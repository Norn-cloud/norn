import { deepStrictEqual, strictEqual } from "node:assert";
import type { Definitions, Json, Query, Runtime, Surface, Surfaces } from "./contract.ts";

export async function runAlpha(definitions: Definitions, surfaces: Surfaces, runtime: Runtime) {
  try {
    const tenant = await runtime.open();
    const compiled = await definitions.compileDefinition(
      "tools/norn-ts/fixtures/engine-alpha/sales-order.source.ts",
    );
    await definitions.checkDefinition(compiled);
    const installed = await definitions.installDefinition(tenant, compiled);
    strictEqual(installed.validation, "valid");
    strictEqual(installed.plan, "additive");
    strictEqual(installed.version.length > 0, true);

    const invoke = async (surface: Surface, action: string, args: { [key: string]: Json }) => {
      strictEqual((await surfaces.invoke(surface, action, args, "operator")).kind, "accepted");
    };
    const snapshot = async (query: Query) => {
      const retained = await runtime.snapshot(query);
      deepStrictEqual(await surfaces.query(query), retained.state);
      strictEqual(new Set(retained.events).size, retained.events.length);
      return retained;
    };
    const refuse = async (surface: Surface, action: string, args: { [key: string]: Json }, query: Query) => {
      const before = await snapshot(query);
      strictEqual((await surfaces.invoke(surface, action, args, "operator")).kind, "refused");
      // Refusal may be audited; accepted business state must remain unchanged.
      deepStrictEqual((await snapshot(query)).state, before.state);
    };

    // Independent records ensure every surface actually executes every action.
    for (const surface of ["api", "mcp", "cli"] satisfies Surface[]) {
      const orderId = `alpha-${surface}`;
      const query = { tenant, orderId };
      const customerId = `customer-${surface}`;
      await invoke(surface, "alpha.customer.create", { id: customerId, name: "Synthetic customer" });
      const create = { id: orderId, order_number: orderId, order_date: "2026-10-31", currency: "USD", counterparty: customerId, total_amount: "10" };
      await invoke(surface, "alpha.order.create", create);
      const initial = await snapshot(query);
      strictEqual(initial.state.status, "draft");
      strictEqual(initial.state.invoiced, false);
      strictEqual(initial.state.completed, false);
      const denied = await surfaces.invoke(surface, "alpha.order.submit", { id: orderId }, "forbidden");
      deepStrictEqual(denied, { kind: "refused", code: "action.permission_denied" });
      deepStrictEqual((await snapshot(query)).state, initial.state);
      await refuse(surface, "alpha.order.approve", { id: orderId }, query);
      await invoke(surface, "alpha.order.submit", { id: orderId });
      await refuse(surface, "alpha.order.record_invoice", { id: `${orderId}-early`, order_id: orderId, amount: "10", reference: "early" }, query);
      await invoke(surface, "alpha.order.approve", { id: orderId });
      const subscription = await surfaces.subscribe(query);
      try {
        // next() consumes the initial subscription snapshot before the mutation.
        deepStrictEqual(await subscription.next(), (await snapshot(query)).state);
        await invoke(surface, "alpha.order.record_invoice", { id: `${orderId}-invoice`, order_id: orderId, amount: "10", reference: "alpha" });
        const invoiced = await snapshot(query);
        deepStrictEqual(invoiced.state, {
          status: "approved", invoice_count: 1, invoiced_total: "10",
          allocated_total: "0", invoiced: true, completed: false, outstanding: "10",
        });
        deepStrictEqual(await subscription.next(), invoiced.state);
        // The same economic fact identity must never be overwritten.
        await refuse(surface, "alpha.order.record_invoice", { id: `${orderId}-invoice`, order_id: orderId, amount: "20", reference: "changed" }, query);
        await invoke(surface, "alpha.order.record_payment", { id: `${orderId}-payment`, order_id: orderId, amount: "4" });
        strictEqual((await snapshot(query)).state.outstanding, "6");
        strictEqual((await snapshot(query)).state.completed, false);
        await refuse(surface, "alpha.order.record_payment", { id: `${orderId}-payment`, order_id: orderId, amount: "8" }, query);
        await invoke(surface, "alpha.order.record_payment", { id: `${orderId}-payment-2`, order_id: orderId, amount: "6" });
        const completed = await snapshot(query);
        deepStrictEqual(completed.state, {
          status: "approved", invoice_count: 1, invoiced_total: "10",
          allocated_total: "10", invoiced: true, completed: true, outstanding: "0",
        });
        deepStrictEqual(completed.events.slice(0, initial.events.length), initial.events);
        strictEqual(completed.events.length > invoiced.events.length, true);
        deepStrictEqual(await runtime.replay(query), completed);
        for (const field of ["invoiced", "completed", "outstanding"]) {
          await refuse(surface, "alpha.order.create", { ...create, id: `${orderId}-computed-${field}`, order_number: `${orderId}-computed-${field}`, [field]: field === "outstanding" ? "999" : false }, query);
        }
        deepStrictEqual(
          await surfaces.invoke(surface, "alpha.order.undeclared", { id: orderId }, "operator"),
          { kind: "refused", code: "action.unknown" },
        );
      } finally {
        await subscription.close();
      }
    }
  } finally {
    await runtime.close();
  }
}
