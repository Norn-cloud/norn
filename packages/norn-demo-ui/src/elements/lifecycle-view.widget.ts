/**
 * Admitted widget document for `<norn-lifecycle-view>` (UI-LIFECYCLE-2, UI-MACHINE-1).
 *
 * Authored in typed TypeScript and lowered to `lifecycle-view.widget.json` (checked by the drift
 * test in `test/lifecycle-view.test.ts`). HTTP 404 / `not_served` enters the dedicated terminal
 * `not_served` state with no Retry button and no 10s retry timer loop (`DESIGN.md` §12.2, §12.4),
 * while transient failures (`network_failure`, `timeout`, `pending_approval`) enter `unavailable`
 * with both manual Retry and a 10s retry timer.
 */
import type { EnvelopeOptions } from "../../../../tools/norn-ts/src/builder/envelope.ts"
import * as W from "../../../../tools/norn-ts/src/builder/widget.ts"

export const LIFECYCLE_VIEW_WIDGET_ID = "norn.studio.lifecycle-view.widget"

const readFacets: W.MachineEffect = {
  kind: "start",
  call: { kind: "operation", operationId: "facetObjects" },
}

export const lifecycleViewWidget: W.WidgetDef = W.widget({
  id: LIFECYCLE_VIEW_WIDGET_ID,
  title: "Lifecycle view",
  version: "0.1.0",
  pages: [
    {
      pageId: "lifecycle",
      name: "Lifecycle",
      root: {
        nodeId: "lifecycle-view",
        component: {
          kind: "section",
          section: {
            sectionId: "lifecycle",
            kind: "section",
            label: { en: "Lifecycle", ar: "دورة الحياة" },
            fields: [],
            order: 0,
          },
        },
        children: [
          {
            nodeId: "retry-button",
            component: {
              kind: "control",
              label: { en: "Try again", ar: "حاول مرة أخرى" },
              input: "trigger",
            },
          },
        ],
      },
      machine: {
        initialStateId: "opening",
        fields: [],
        events: [
          { eventId: "opened", source: { kind: "page_opened" } },
          { eventId: "retry", source: { kind: "user", nodeId: "retry-button" } },
          { eventId: "changed", source: { kind: "subscription", bindingId: "subject" } },
          { eventId: "again", source: { kind: "timer", afterMs: 10000 } },
          W.outcome("accepted", "accepted"),
          W.outcome("refused", "refused"),
          W.outcome("pending-approval", "pending_approval"),
          W.outcome("network-failure", "network_failure"),
          W.outcome("timed-out", "timeout"),
          W.outcome("not-served", "not_served"),
        ],
        states: [
          { stateId: "opening", role: "idle" },
          {
            stateId: "reading",
            role: "pending",
            awaits: { kind: "operation", operationId: "facetObjects" },
            deadlineMs: 15000,
          },
          { stateId: "showing", role: "idle" },
          { stateId: "refused", role: "idle" },
          { stateId: "unavailable", role: "error" },
          { stateId: "not_served", role: "terminal" },
        ],
        transitions: [
          { transitionId: "open", from: "opening", on: "opened", target: "reading", effects: [readFacets] },
          { transitionId: "read", from: "reading", on: "accepted", target: "showing" },
          { transitionId: "read-refused", from: "reading", on: "refused", target: "refused" },
          { transitionId: "read-pending", from: "reading", on: "pending-approval", target: "unavailable" },
          { transitionId: "read-failed", from: "reading", on: "network-failure", target: "unavailable" },
          { transitionId: "read-timeout", from: "reading", on: "timed-out", target: "unavailable" },
          { transitionId: "read-not-served", from: "reading", on: "not-served", target: "not_served" },
          { transitionId: "changed", from: "showing", on: "changed", target: "reading", effects: [readFacets] },
          { transitionId: "refused-changed", from: "refused", on: "changed", target: "reading", effects: [readFacets] },
          { transitionId: "refused-retry", from: "refused", on: "retry", target: "reading", effects: [readFacets] },
          { transitionId: "unavailable-retry", from: "unavailable", on: "retry", target: "reading", effects: [readFacets] },
          { transitionId: "unavailable-again", from: "unavailable", on: "again", target: "reading", effects: [readFacets] },
        ],
      },
    },
  ],
  contract: {
    dataBindings: [
      {
        bindingId: "subject",
        object: { id: "norn.studio.lifecycle-subject.entity", version: "0.1.0" },
        query: {
          kind: "object_list",
          filters: [],
          sort: [],
          page: { size: 1, maxSize: 1 },
        },
      },
    ],
  },
})

export const lifecycleViewOptions: EnvelopeOptions = {
  scope: { orgId: "org_demo", environmentId: "dev" },
  sourcePath: "packages/norn-demo-ui/src/elements/lifecycle-view.widget.ts",
}

/** Lower the lifecycle view widget definition to its canonical IR JSON document. */
export const lifecycleViewArtifact = () => W.lowerWidget(lifecycleViewWidget, lifecycleViewOptions)
