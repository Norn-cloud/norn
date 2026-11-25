/**
 * The fixture Widget UX page the state-matrix harness proves `<NornAppRenderer>` on (#1762
 * milestone C), written with the typed widget builder (`tools/norn-ts`, `W`) and admitted by the
 * checker against the recorded demo definitions (`fixtures/demo/explorer`). It reads one bounded
 * page of records, edits a note through a ZEN assignment (the lazy engine's path), and saves
 * through a ledgered action behind a UI SDK alert dialog held open while it is asked and sent. It
 * places one island of each tier.
 *
 * Every pending state handles every outcome, and every error state offers a person's Retry or
 * Close (L2, L4); the checker proves it (`test/matrix-page.test.ts`).
 */
import { readFileSync } from "node:fs"
import { join } from "node:path"
import * as W from "../../../../tools/norn-ts/src/builder/widget.ts"

export const MATRIX_WIDGET_ID = "fixture.matrix.widget"
export const ENTITY = { id: "canon.ValuationPolicy.entity", version: "0.1.0" } as const
export const ACTION = { id: "canon.valuation_policy.set.action", version: "0.1.0" } as const
export const OBJECT_TYPE = "canon.ValuationPolicy"
export const ACTION_ID = "canon.valuation_policy.set"
export const SCOPE = { orgId: "org_demo", environmentId: "analytics" } as const

const label = (en: string, ar: string) => ({ en, ar })
const control = (nodeId: string, en: string, ar: string, input: "trigger" | "text" = "trigger"): W.WidgetNode => ({ nodeId, component: { kind: "control", label: label(en, ar), input } })
const user = (eventId: string, nodeId: string, payload?: ReadonlyArray<W.MachineField>): W.MachineEvent => ({ eventId, source: { kind: "user", nodeId }, ...(payload === undefined ? {} : { payload }) })
const failures = ["refused", "network_failure", "not_served", "timeout"] as const
const readStart = { kind: "start", call: { kind: "read", bindingId: "records" } } as const
const saveStart = { kind: "start", call: { kind: "action", bindingId: "save" } } as const

export const matrixWidget: W.WidgetDef = W.widget({
  id: MATRIX_WIDGET_ID,
  title: "Matrix fixture",
  version: "0.1.0",
  dependencies: [{ artifactId: ENTITY.id, version: ENTITY.version, kind: "entity" }, { artifactId: ACTION.id, version: ACTION.version, kind: "action" }],
  pages: [{
    pageId: "records",
    name: "Records",
    view: { kind: "collection", subject: { kind: "object_type", object: ENTITY } },
    root: {
      nodeId: "root",
      component: { kind: "section", section: { sectionId: "main", kind: "section", label: label("Records", "السجلات"), fields: [], order: 1 } },
      children: [
        { nodeId: "records-list", component: { kind: "list", dataBindingId: "records", selection: "none", fields: [{ bindingId: "records", propertyId: "label" }, { bindingId: "records", propertyId: "valuation_method" }] } },
        control("retry-read", "Read again", "اقرأ مرة أخرى"),
        control("note-input", "Note", "ملاحظة", "text"),
        control("save-node", "Save", "حفظ"),
        control("close-node", "Close", "إغلاق"),
        control("retry-save", "Try again", "حاول مرة أخرى"),
        W.uiSdk("confirm-layer", "alert-dialog", {
          heldIn: ["confirming", "saving"],
          props: { pendingAnnouncement: '"Saving"' },
          children: [
            { nodeId: "confirm-title", component: { kind: "section", section: { sectionId: "confirm", kind: "section", label: label("Save this record?", "حفظ هذا السجل؟"), fields: [], order: 2 } } },
            // The save call's one placement: a person's press on it is the machine's `confirm`.
            { nodeId: "confirm-node", component: { kind: "action", placementId: "confirm-save" } },
            control("cancel-node", "Cancel", "إلغاء"),
          ],
        }),
        { nodeId: "meter-node", component: { kind: "catalog", entry: { entryId: "fixture-meter" }, props: { caption: '"Records read"' } } },
        { nodeId: "status-node", component: { kind: "catalog", entry: { entryId: "fixture-status" } } },
      ],
    },
    machine: {
      initialStateId: "opening",
      fields: [{ fieldId: "note", type: "text", required: true, initial: "" }],
      events: [
        { eventId: "opened", source: { kind: "page_opened" } },
        user("retry-read", "retry-read"),
        user("edit-note", "note-input", [{ fieldId: "value", type: "text", required: true }]),
        user("save", "save-node"),
        user("confirm", "confirm-node"),
        user("cancel", "cancel-node"),
        user("close", "close-node"),
        user("retry-save", "retry-save"),
        { eventId: "picked", source: { kind: "user", nodeId: "meter-node", emits: "pick" } },
        // Escape (or the layer's own close) while it asks: the layer reports it; while saving, nothing takes it.
        { eventId: "dismissed", source: { kind: "user", nodeId: "confirm-layer", emits: "onOpenChange" }, payload: [{ fieldId: "value", type: "boolean", required: true }] },
        W.outcome("accepted", "accepted"),
        W.outcome("answer-moved", "answer_moved"),
        W.outcome("pending-approval", "pending_approval"),
        ...failures.map((failure) => W.outcome(failure, failure)),
      ],
      states: [
        { stateId: "opening", role: "idle" },
        { stateId: "loading", role: "pending", awaits: { kind: "read", bindingId: "records" }, deadlineMs: 15000 },
        { stateId: "read-failed", role: "error" },
        { stateId: "browsing", role: "idle" },
        { stateId: "confirming", role: "idle" },
        { stateId: "saving", role: "pending", awaits: { kind: "action", bindingId: "save" }, deadlineMs: 15000 },
        { stateId: "saved", role: "idle" },
        { stateId: "awaiting-approval", role: "idle" },
        { stateId: "save-failed", role: "error" },
      ],
      transitions: [
        { transitionId: "open", from: "opening", on: "opened", target: "loading", effects: [readStart] },
        { transitionId: "read-accepted", from: "loading", on: "accepted", target: "browsing" },
        { transitionId: "read-moved", from: "loading", on: "answer-moved", target: "read-failed" },
        ...failures.map((failure) => ({ transitionId: `read-${failure}`, from: "loading", on: failure, target: "read-failed" })),
        { transitionId: "read-again", from: "read-failed", on: "retry-read", target: "loading", effects: [readStart] },
        { transitionId: "reread", from: "browsing", on: "retry-read", target: "loading", effects: [readStart] },
        { transitionId: "edit-note", from: "browsing", on: "edit-note", target: "browsing", assign: { note: "input.event.value" } },
        { transitionId: "pick", from: "browsing", on: "picked", target: "browsing" },
        { transitionId: "ask", from: "browsing", on: "save", target: "confirming" },
        { transitionId: "confirm", from: "confirming", on: "confirm", target: "saving", effects: [saveStart] },
        { transitionId: "cancel", from: "confirming", on: "cancel", target: "browsing" },
        { transitionId: "dismiss", from: "confirming", on: "dismissed", target: "browsing" },
        { transitionId: "save-accepted", from: "saving", on: "accepted", target: "saved" },
        { transitionId: "save-pending", from: "saving", on: "pending-approval", target: "awaiting-approval" },
        ...failures.map((failure) => ({ transitionId: `save-${failure}`, from: "saving", on: failure, target: "save-failed" })),
        { transitionId: "saved-close", from: "saved", on: "close", target: "browsing" },
        { transitionId: "approval-close", from: "awaiting-approval", on: "close", target: "browsing" },
        { transitionId: "save-again", from: "save-failed", on: "retry-save", target: "saving", effects: [saveStart] },
        { transitionId: "save-close", from: "save-failed", on: "close", target: "browsing" },
      ],
    },
  }],
  actionBindings: [{
    bindingId: "save",
    action: ACTION,
    inputMapping: { id: '"fixture-1"', label: '"Fixture policy"', valuation_method: '"fifo"' },
    requiredCapability: "canon.valuation_policy.set",
  }],
  contract: {
    dataBindings: [{ bindingId: "records", object: ENTITY, query: { kind: "object_list", filters: [], sort: [], page: { size: 20, maxSize: 50 } } }],
    // The save call's one placement, inside its dialog scope; the page machine starts it.
    actionPlacements: [{
      placementId: "confirm-save", bindingId: "save", label: label("Confirm", "تأكيد"), location: "toolbar", order: 1,
      selectionInputMapping: {}, confirmation: { severity: "safe", strength: "none", message: label("Saving records the change in the ledger.", "الحفظ يسجّل التغيير في الدفتر.") },
    }],
    fields: [
      { fieldId: "label-field", target: { kind: "property", property: { bindingId: "records", propertyId: "label" } }, label: label("Name", "الاسم"), control: "display" },
      { fieldId: "method-field", target: { kind: "property", property: { bindingId: "records", propertyId: "valuation_method" } }, label: label("Method", "الطريقة"), control: "display" },
    ],
    catalog: [
      { entryId: "fixture-meter", label: label("Meter", "عداد"), trust: "opaque_island", framework: "react", props: [{ fieldId: "caption", type: "text", required: true }], events: [{ eventId: "pick" }] },
      { entryId: "fixture-status", label: label("Status", "الحالة"), trust: "contract_checked", framework: "react" },
    ],
  },
})

export const matrixOptions = { scope: SCOPE, sourcePath: "packages/norn-react/test/fixtures/matrix-page.ts" }

/** The fixture as its admitted `widget` artifact. */
export const matrixArtifact = (): { readonly kind: string; readonly artifactId: string; readonly payload: { readonly ux: unknown } } =>
  W.lowerWidget(matrixWidget, matrixOptions) as never

/** The recorded demo definitions the fixture's bindings resolve against. */
export function demoDefinitions(): unknown[] {
  return JSON.parse(readFileSync(join(import.meta.dirname, "../../../../fixtures/demo/explorer/three-systems.definitions.json"), "utf8")) as unknown[]
}
