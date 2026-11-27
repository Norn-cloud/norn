/**
 * `@norn/react`: the React renderer of admitted Widget UX documents (#1762 milestone C).
 *
 * - `NornAppRenderer` renders one page of an admitted app in the decided workspace.
 * - `useWidgetMachine` runs a page's `WidgetMachine`: guard-free transitions in TypeScript, ZEN guards
 *   and assignments in the checker's engine, loaded lazily after the first paint.
 * - `state.ts` holds the closed UI states (`WorkspaceState`, `ReadState`, `ActionCallState`) and their
 *   exhaustive matchers; `machine/` the framework-free machine runtime.
 * - `capabilities.ts` says what the renderer draws, and refuses a page that needs more (L6).
 */
export { AGENT_CAPABILITIES, missingCapabilities, primitiveOf, RENDERED_BEHAVIORS, RENDERED_COMPONENTS, RENDERED_PRIMITIVES, RENDERER_CAPABILITIES, requiredCapabilities, unsupported } from "./capabilities.ts"
export { confirmMark, confirmReason, effectsReason, rowMark, UNKNOWN_EFFECTS, valueMark, type ActionEffects, type AgentAttributes, type MaskedProperty } from "./agent.ts"
export { islandsOf, tierOf, type IslandInput, type IslandRegistry, type IslandStates, type IslandTier } from "./islands.ts"
export { bindingRead, callStateOf, contextRead, readStateOf, refusalOf, type BindingCallState, type BindingRead, type BindingReadState, type Refusal, type WidgetCalls, type WidgetLive, type WidgetReader, type WidgetReaderQuery } from "./reads.ts"
export { NornAppRenderer, pageKey, refusalsOf, type NornAppRendererProps } from "./renderer.tsx"
export { createPageRuntime, ENGINE_WAIT_MS, needsEngine, useWidgetMachine, type EngineState, type MachineOptions, type MachineRuntime, type PageRuntime, type RuntimeView } from "./runtime.ts"
export * from "./state.ts"
export { text, type Message, type Text, type Translate, type WidgetNode, type WidgetPage, type WidgetUx } from "./ux.ts"
