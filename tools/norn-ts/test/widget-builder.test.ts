// Widget UX authoring (#1762): a typed page lowers to a `widget` artifact, and the Rust checker's
// laws judge it. UI SDK primitives, options and callbacks are typed from the pinned catalog.
import { expect, test } from "bun:test"
import { W } from "../src/builder/index.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"
if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "widgets/confirm.ts" }
const label = (en: string, ar: string) => ({ en, ar })
const control = (nodeId: string, en: string, ar: string): W.WidgetNode => ({ nodeId, component: { kind: "control", label: label(en, ar), input: "trigger" } })

/** A confirmation held open by an alert dialog while it is asked and while its call is in flight. */
function confirmPage(heldIn: ReadonlyArray<string>, cancel: boolean): W.WidgetDef {
  const failures = ["refused", "pending_approval", "network_failure", "not_served", "timeout"] as const
  return W.widget({
    id: "demo.confirm.widget",
    title: "Confirm",
    version: "0.1.0",
    pages: [{
      pageId: "confirm",
      name: "Confirm",
      root: {
        nodeId: "root",
        component: { kind: "section", section: { sectionId: "main", kind: "section", label: label("Confirm", "تأكيد"), fields: [], order: 1 } },
        children: [
          control("open-node", "Send", "إرسال"),
          W.uiSdk("confirm-layer", "alert-dialog", {
            heldIn,
            props: { pendingAnnouncement: '"Sending"' },
            children: [control("confirm-node", "Confirm", "تأكيد"), ...(cancel ? [control("cancel-node", "Cancel", "إلغاء")] : [])],
          }),
        ],
      },
      machine: {
        initialStateId: "browsing",
        events: [
          { eventId: "open", source: { kind: "user", nodeId: "open-node" } },
          { eventId: "confirm", source: { kind: "user", nodeId: "confirm-node" } },
          ...(cancel ? [{ eventId: "cancel", source: { kind: "user", nodeId: "cancel-node" } } as const] : []),
          W.outcome("accepted", "accepted"),
          ...failures.map((failure) => W.outcome(failure, failure)),
        ],
        states: [
          { stateId: "browsing", role: "idle" },
          { stateId: "confirming", role: "idle" },
          { stateId: "sending", role: "pending", deadlineMs: 15000, awaits: { kind: "operation", operationId: "decideDecision" } },
          { stateId: "failed", role: "error" },
        ],
        transitions: [
          { transitionId: "open", from: "browsing", on: "open", target: "confirming" },
          { transitionId: "confirm", from: "confirming", on: "confirm", target: "sending", effects: [{ kind: "start", call: { kind: "operation", operationId: "decideDecision" } }] },
          ...(cancel ? [{ transitionId: "cancel", from: "confirming", on: "cancel", target: "browsing" }] : []),
          { transitionId: "accepted", from: "sending", on: "accepted", target: "browsing" },
          ...failures.map((failure) => ({ transitionId: `on-${failure}`, from: "sending", on: failure, target: "failed" })),
          { transitionId: "close", from: "failed", on: "open", target: "browsing" },
        ],
      },
    }],
  })
}

const codes = (def: W.WidgetDef): string[] => {
  const checked = checker!.checkArtifactSet([W.lowerWidget(def, options)])
  return checked.diagnostics.diagnostics.filter((d) => d.severity === "error").map((d) => d.code)
}

test("a typed page lowers to the widget artifact the checker reads", () => {
  const artifact = W.lowerWidget(confirmPage(["confirming", "sending"], true), options) as {
    kind: string; payload: { ux: { pages: { root: { children: { component: unknown }[] }; machine: { machineVersion: string } }[] } }
  }
  expect(artifact.kind).toBe("widget")
  expect(artifact.payload.ux.pages[0]!.machine.machineVersion).toBe("0.1")
  expect(artifact.payload.ux.pages[0]!.root.children[1]!.component).toEqual({
    kind: "catalog", entry: { entryId: "ui-sdk:alert-dialog" }, props: { pendingAnnouncement: '"Sending"' }, heldIn: ["confirming", "sending"],
  })
  expect(W.UI_SDK_VERSION).toMatch(/^\d+\.\d+\.\d+/)
  // Typed from the catalog: an unknown option, a callback as a prop, and an unknown primitive do not compile.
  // @ts-expect-error not an option of alert-dialog
  W.uiSdk("x", "alert-dialog", { props: { sideways: "true" } })
  // @ts-expect-error a callback is raised as an event, never bound
  W.uiSdk("x", "dialog", { props: { onOpenChange: "true" } })
  // @ts-expect-error not a UI SDK primitive
  W.uiSdk("x", "no-such-primitive")
  expect(W.fromUiSdk("ask", "confirm-layer", "dialog", "onDismissAttempt").source).toEqual({ kind: "user", nodeId: "confirm-layer", emits: "onDismissAttempt" })
})

test.skipIf(!checker)("the checker admits the held confirmation and refuses it without a Cancel (L1)", () => {
  expect(codes(confirmPage(["confirming", "sending"], true))).toEqual([])
  expect(codes(confirmPage(["confirming", "sending"], false))).toContain("widget.layer.dismissal_refused_without_cause")
})
