import { Link, useParams } from "@tanstack/react-router"
import { Background, BaseEdge, Controls, EdgeLabelRenderer, Handle, MarkerType, Position, ReactFlow, getSmoothStepPath, type Edge, type EdgeProps, type Node, type NodeProps } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { ArrowRight, GitBranch, RotateCcw, ShieldCheck, SlidersHorizontal, Workflow } from "lucide-react"
import { useState } from "react"
import { Chip, type Tone } from "@/components/chip"
import { Input } from "@/components/ui/input"
import { cn } from "@/lib/utils"
import type { MenuEntry } from "../shell.tsx"
import { COMMANDS, MACHINES, PROCESS_MANAGERS, type FinanceMachine, type State, type StatePhase, type StateStep } from "./behavior.ts"
import { ROW_COUNT, TYPES } from "./data.ts"
import { RULES } from "./operations.tsx"
import { FinanceHeading, SystemDependencies, TypeTile } from "./workspace.tsx"

type Direction = "forward" | "down" | "up" | "return-down" | "return-up"

type StateNode = Node<State, "state">
type TransitionEdge = Edge<{
  readonly step: StateStep
  readonly direction: Direction
  readonly lift: number
  readonly selected: boolean
  readonly choose: (step: StateStep) => void
}, "transition">

const STATE_TONE: Record<State["kind"], Tone> = { initial: "green", active: "blue", review: "amber", terminal: "gray", refused: "red" }
const STATE_KIND: Record<State["kind"], string> = { initial: "Start", active: "State", review: "Review", terminal: "End", refused: "Refused" }
const HANDLE = "!opacity-0 !border-0 !bg-transparent"

/** High-level domain parameters a domain expert can tune per transition without touching code. */
interface StepParam {
  readonly key: string
  readonly label: string
  readonly value: number
  readonly unit: string
  readonly step?: number
}

const STEP_PARAMS: Readonly<Record<string, readonly StepParam[]>> = {
  disburse: [
    { key: "dual_approval", label: "Dual-approval threshold", value: 1_000_000, unit: "EGP", step: 100_000 },
    { key: "bureau_days", label: "I-Score freshness window", value: 30, unit: "days", step: 5 },
  ],
  autoApprove: [
    { key: "min_score", label: "Auto-approve I-Score floor", value: 650, unit: "score", step: 10 },
    { key: "max_dti", label: "Max DTI ceiling", value: 50, unit: "%", step: 5 },
  ],
  review: [
    { key: "review_floor", label: "Manual review I-Score floor", value: 600, unit: "score", step: 10 },
  ],
  retryKyc: [
    { key: "max_retries", label: "Max KYC retries", value: 2, unit: "attempts", step: 1 },
  ],
  advanceArrears: [
    { key: "late_dpd", label: "Late arrears trigger", value: 31, unit: "DPD", step: 5 },
    { key: "severe_dpd", label: "Severe arrears trigger", value: 90, unit: "DPD", step: 10 },
  ],
  writeOff: [
    { key: "writeoff_dpd", label: "Write-off eligibility", value: 90, unit: "DPD", step: 10 },
  ],
  openMargin: [
    { key: "margin_score", label: "Margin suitability I-Score", value: 650, unit: "score", step: 10 },
  ],
  screenPayer: [
    { key: "aml_cap", label: "Monthly third-party payer cap", value: 50_000, unit: "EGP", step: 10_000 },
  ],
}

function StateNodeView({ data }: NodeProps<StateNode>) {
  return (
    <div className="canvas-node flex h-11 w-44 items-center gap-2 rounded-md border border-border bg-background px-2.5 text-foreground">
      <Handle type="target" position={Position.Left} id="in" className={HANDLE} />
      <Handle type="source" position={Position.Right} id="out" className={HANDLE} />
      <Handle type="target" position={Position.Bottom} id="bottom-in" className={HANDLE} />
      <Handle type="source" position={Position.Bottom} id="bottom-out" className={HANDLE} />
      <Handle type="target" position={Position.Top} id="top-in" className={HANDLE} />
      <Handle type="source" position={Position.Top} id="top-out" className={HANDLE} />
      <span aria-label={STATE_KIND[data.kind]} className="size-2 shrink-0 rounded-full" style={{ background: `var(--tone-${STATE_TONE[data.kind]})` }} />
      <span className="truncate text-xs font-medium">{data.name}</span>
    </div>
  )
}

/** Compact Retool-style edge pills: command name only on canvas (no multi-line overlap!), full guard in the right inspector. */
function TransitionView({ id, sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition, markerEnd, data }: EdgeProps<TransitionEdge>) {
  if (!data) return null
  const { step, direction, lift, selected, choose } = data
  const reverse = direction === "return-down" || direction === "return-up"
  const vertical = direction === "up" || direction === "down"
  const upper = direction === "return-up"
  const [forward, x, y] = getSmoothStepPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition, borderRadius: 8, offset: 12 })
  const turnY = upper ? Math.min(sourceY, targetY) - lift : Math.max(sourceY, targetY) + lift
  const d = reverse
    ? `M ${sourceX} ${sourceY} C ${sourceX} ${turnY}, ${sourceX} ${turnY}, ${sourceX - 18} ${turnY} H ${targetX + 18} C ${targetX} ${turnY}, ${targetX} ${turnY}, ${targetX} ${targetY}`
    : lift > 0 ? `M ${sourceX} ${sourceY} C ${sourceX + 55} ${sourceY + lift}, ${targetX - 55} ${targetY + lift}, ${targetX} ${targetY}` : forward
  const labelX = reverse ? (sourceX + targetX) / 2 : vertical ? x + (x < 210 ? 58 : -58) : lift > 0 ? x + 46 : x
  const labelY = reverse ? turnY + (upper ? -12 : 12) : vertical ? y : lift > 0 ? y + 28 : y - (sourceY === targetY || Math.abs(sourceY - targetY) > 190 ? 18 : 14)

  return (
    <>
      <BaseEdge
        id={id}
        path={d}
        markerEnd={markerEnd}
        style={{ stroke: selected ? "var(--data-accent)" : "var(--edge)", strokeWidth: selected ? 2.2 : 1.35, strokeDasharray: reverse ? "4 3" : undefined }}
      />
      <EdgeLabelRenderer>
        <button
          type="button"
          aria-pressed={selected}
          onClick={() => choose(step)}
          title={`${step.command} · ${step.guard}`}
          className={cn(
            "nodrag nopan pointer-events-auto absolute z-10 rounded-sm border px-1.5 py-0.5 font-mono text-[10px] font-medium whitespace-nowrap transition-colors",
            selected
              ? "border-data-accent bg-data-accent text-white shadow-xs"
              : "border-border bg-background text-foreground/85 hover:border-data-accent hover:text-tone-blue",
          )}
          style={{ transform: `translate(-50%, -50%) translate(${labelX}px,${labelY}px)` }}
        >
          {step.command}
        </button>
      </EdgeLabelRenderer>
    </>
  )
}

const NODE_TYPES = { state: StateNodeView }
const EDGE_TYPES = { transition: TransitionView }

function BranchGraph({ phase, selected, onSelect }: { readonly phase: StatePhase; readonly selected: StateStep | undefined; readonly onSelect: (step: StateStep) => void }) {
  const nodes: StateNode[] = Object.entries(phase.states).map(([id, data]) => ({
    id, type: "state", data, position: { x: data.column * 232 + 48, y: data.row * 136 + 72 },
  }))
  const edges: TransitionEdge[] = phase.steps.flatMap((step, index) => {
    const source = phase.states[step.from]
    const target = phase.states[step.to]
    if (!source || !target) return []
    const direction: Direction = source.column < target.column ? "forward"
      : source.column === target.column ? source.row > target.row ? "up" : "down"
        : step.command === "activatePlan" || step.command === "keepPromise" ? "return-up" : "return-down"
    const parallel = phase.steps.slice(0, index).filter((previous) => previous.from === step.from && previous.to === step.to).length
    const lift = direction === "return-up" ? (step.command === "activatePlan" ? 36 : 74) : direction === "return-down" ? 34 + (index % 3) * 18 : parallel * 44
    const sourceHandle = direction === "forward" ? "out" : direction === "up" || direction === "return-up" ? "top-out" : "bottom-out"
    const targetHandle = direction === "forward" ? "in" : direction === "down" || direction === "return-up" ? "top-in" : "bottom-in"
    return [{
      id: `${step.from}-${step.to}-${step.command}`,
      source: step.from,
      target: step.to,
      sourceHandle,
      targetHandle,
      data: { step, direction, lift, selected: step === selected, choose: onSelect },
      type: "transition",
      markerEnd: { type: MarkerType.ArrowClosed, color: step === selected ? "var(--data-accent)" : "var(--edge)", width: 13, height: 13 },
    }]
  })
  const lastRow = Math.max(...Object.values(phase.states).map((state) => state.row))
  const returnAtBottom = edges.some(({ data }) => data?.direction === "return-down" && phase.states[data.step.from]?.row === lastRow)
  return (
    <div aria-label={`${phase.title} state graph`} className="h-[34rem] w-full bg-canvas-surface">
      <ReactFlow
        nodes={nodes}
        edges={edges}
        nodeTypes={NODE_TYPES}
        edgeTypes={EDGE_TYPES}
        fitView
        fitViewOptions={{ padding: { top: "56px", bottom: returnAtBottom ? "68px" : "24px", left: "24px", right: "24px" }, maxZoom: 1.1 }}
        minZoom={0.35}
        maxZoom={2}
        nodesDraggable={false}
        nodesConnectable={false}
        nodesFocusable={false}
        edgesFocusable={false}
        zoomOnScroll={false}
        zoomOnDoubleClick={false}
        panOnScroll={false}
        preventScrolling={false}
        proOptions={{ hideAttribution: true }}
      >
        <Background gap={18} size={1} />
        <Controls showInteractive={false} position="bottom-left" />
      </ReactFlow>
    </div>
  )
}

function PhaseWorkbench({ phase }: { readonly phase: StatePhase }) {
  const [selected, setSelected] = useState<StateStep>(phase.steps[0]!)
  const [inspectorTab, setInspectorTab] = useState<"inspector" | "list">("inspector")
  const [drafts, setDrafts] = useState<Readonly<Record<string, number>>>({})

  const declaration = selected.declared ? COMMANDS[selected.declared] : undefined
  const params = STEP_PARAMS[selected.command] ?? []
  const draftCount = Object.keys(drafts).length

  return (
    <div className="grid overflow-hidden rounded-lg border border-border bg-background lg:grid-cols-[minmax(0,1fr)_22rem]">
      <div className="min-w-0">
        <BranchGraph phase={phase} selected={selected} onSelect={(step) => { setSelected(step); setInspectorTab("inspector") }} />
      </div>

      <aside aria-label="Transition inspector" className="flex flex-col border-t border-border bg-background lg:h-[34rem] lg:border-s lg:border-t-0">
        <div className="flex items-center justify-between border-b border-border bg-surface-subtle px-3">
          <div role="tablist" className="flex gap-1">
            <button
              type="button"
              role="tab"
              aria-selected={inspectorTab === "inspector"}
              onClick={() => setInspectorTab("inspector")}
              className={cn("border-b-2 px-2.5 py-2.5 text-xs font-medium", inspectorTab === "inspector" ? "border-data-accent text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}
            >
              Inspector
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={inspectorTab === "list"}
              onClick={() => setInspectorTab("list")}
              className={cn("border-b-2 px-2.5 py-2.5 text-xs font-medium", inspectorTab === "list" ? "border-data-accent text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}
            >
              Transitions ({phase.steps.length})
            </button>
          </div>
          {draftCount > 0 && (
            <button type="button" onClick={() => setDrafts({})} className="inline-flex items-center gap-1 text-[11px] font-medium text-tone-blue hover:underline">
              <RotateCcw className="size-3" />Reset ({draftCount})
            </button>
          )}
        </div>

        {inspectorTab === "inspector" ? (
          <div className="flex-1 space-y-4 overflow-y-auto p-4 text-xs">
            <div className="space-y-1">
              <p className="text-[10px] font-semibold tracking-wider uppercase text-muted-foreground">Selected transition</p>
              <code className="block text-sm font-semibold text-tone-blue">{selected.declared ?? selected.command}</code>
              <p className="text-muted-foreground">
                <span className="font-medium text-foreground">{phase.states[selected.from]?.name}</span> → <span className="font-medium text-foreground">{phase.states[selected.to]?.name}</span>
              </p>
            </div>

            <div className="space-y-1 border-t border-border pt-3">
              <p className="text-[11px] font-medium text-muted-foreground">Guard condition</p>
              <p className="text-xs leading-relaxed text-foreground">{selected.guard}</p>
            </div>

            {params.length > 0 && (
              <div className="space-y-2.5 rounded-md border border-border bg-surface-subtle p-3">
                <div className="flex items-center justify-between">
                  <span className="inline-flex items-center gap-1.5 text-[11px] font-semibold text-foreground">
                    <SlidersHorizontal className="size-3 text-muted-foreground" />Domain parameters
                  </span>
                  <span className="text-[10px] text-muted-foreground">No-code draft</span>
                </div>
                {params.map((param) => {
                  const k = `${selected.command}:${param.key}`
                  const val = drafts[k] ?? param.value
                  const changed = val !== param.value
                  return (
                    <label key={param.key} className="block space-y-1">
                      <span className="flex items-center justify-between text-[11px] text-muted-foreground">
                        <span>{param.label}</span>
                        {changed && <span className="font-medium text-tone-blue">Modified</span>}
                      </span>
                      <div className="flex items-center gap-2">
                        <Input
                          type="number"
                          min={1}
                          step={param.step ?? 1}
                          value={val}
                          onChange={(e) => {
                            const next = Number(e.target.value)
                            if (Number.isFinite(next) && next > 0) setDrafts((curr) => ({ ...curr, [k]: next }))
                          }}
                          className="h-7 bg-background font-mono text-xs"
                        />
                        <span className="shrink-0 text-[11px] text-muted-foreground">{param.unit}</span>
                      </div>
                    </label>
                  )
                })}
                {params.some((p) => drafts[`${selected.command}:${p.key}`] !== undefined && drafts[`${selected.command}:${p.key}`] !== p.value) && (
                  <p role="status" className="text-[11px] text-tone-blue">
                    Draft threshold active in preview — validated against invariants on publish.
                  </p>
                )}
              </div>
            )}

            {declaration && declaration.rules.length > 0 && (
              <div className="space-y-1.5 border-t border-border pt-3">
                <p className="text-[11px] font-medium text-muted-foreground">Enforced invariants</p>
                <div className="flex flex-col gap-1.5">
                  {declaration.rules.map((rule) => (
                    <Link key={rule} to="/guarantees" hash={rule} className="flex items-center justify-between rounded-md border border-border bg-surface-subtle px-2.5 py-1.5 font-medium text-foreground hover:border-data-accent hover:text-tone-blue">
                      <span className="inline-flex items-center gap-1.5"><ShieldCheck className="size-3.5 text-tone-blue" />{RULES.find((item) => item.id === rule)?.title ?? rule}</span>
                      <ArrowRight className="size-3 text-muted-foreground" />
                    </Link>
                  ))}
                  {selected.declared === "Payment.reclassify" && (
                    <Link to="/metrics" hash="corrections" className="flex items-center justify-between rounded-md border border-border bg-surface-subtle px-2.5 py-1.5 font-medium text-tone-amber hover:border-tone-amber">
                      <span>View reconciliation correction</span>
                      <ArrowRight className="size-3" />
                    </Link>
                  )}
                </div>
              </div>
            )}

            <div className="border-t border-border pt-3">
              <p className="mb-1.5 text-[11px] font-medium text-muted-foreground">Backing systems</p>
              <SystemDependencies systems={selected.systems} needs={selected.needs} />
            </div>
          </div>
        ) : (
          <ul className="flex-1 divide-y divide-border overflow-y-auto">
            {phase.steps.map((step) => {
              const active = step === selected
              return (
                <li key={`${step.from}-${step.to}-${step.command}`}>
                  <button
                    type="button"
                    aria-pressed={active}
                    onClick={() => { setSelected(step); setInspectorTab("inspector") }}
                    className={cn("w-full px-3.5 py-2 text-start transition-colors hover:bg-surface-subtle", active && "border-s-2 border-s-data-accent bg-tone-blue/[0.06]")}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <code className="text-xs font-semibold text-tone-blue">{step.command}</code>
                      <span className="truncate text-[10px] text-muted-foreground">{phase.states[step.from]?.name} → {phase.states[step.to]?.name}</span>
                    </div>
                    <p className="mt-0.5 truncate text-[11px] text-muted-foreground">{step.guard}</p>
                  </button>
                </li>
              )
            })}
          </ul>
        )}
      </aside>
    </div>
  )
}

export function FinanceStateMachine({ machine }: { readonly machine: FinanceMachine }) {
  const [phaseName, setPhaseName] = useState(machine.phases[0].title)
  const phase = machine.phases.find((item) => item.title === phaseName) ?? machine.phases[0]
  return (
    <section className="space-y-3" aria-label={machine.title}>
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border bg-background px-3.5 py-2">
        <div className="flex items-center gap-2.5">
          <TypeTile type={machine.type} />
          <div className="flex items-baseline gap-2">
            <h2 className="text-sm font-semibold">{machine.title}</h2>
            <code className="text-[11px] text-muted-foreground">{machine.property}</code>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-3">
          <div aria-label="State kinds" className="hidden items-center gap-3 text-[11px] text-muted-foreground md:flex">
            {(["initial", "review", "terminal", "refused"] as const).map((kind) => (
              <span key={kind} className="inline-flex items-center gap-1.5">
                <span className="size-1.5 rounded-full" style={{ background: `var(--tone-${STATE_TONE[kind]})` }} />
                {STATE_KIND[kind]}
              </span>
            ))}
          </div>
          {machine.phases.length > 1 && (
            <div role="group" aria-label="Lifecycle phase" className="inline-flex rounded-md border border-border bg-surface-subtle p-0.5 text-xs">
              {machine.phases.map((item) => (
                <button
                  key={item.title}
                  type="button"
                  aria-pressed={item === phase}
                  onClick={() => setPhaseName(item.title)}
                  className={cn(
                    "rounded-sm px-2.5 py-1 font-medium transition-colors",
                    item === phase ? "bg-background text-foreground shadow-xs ring-1 ring-border" : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  {item.title}
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      <PhaseWorkbench key={phase.title} phase={phase} />
    </section>
  )
}

export const financeWorkflowMenu = (): readonly MenuEntry[] => [
  ...MACHINES.map(({ id, type }) => ({ href: `/workflows/${id}`, label: TYPES[type].name, group: "State machines" })),
  ...PROCESS_MANAGERS.map(({ id, title }) => ({ href: `/workflows/${id}`, label: title, group: "Automations" })),
]

/** Retool-style horizontal automation rules with live enable/pause toggle and right inspector. */
function ProcessManager({ manager }: { readonly manager: typeof PROCESS_MANAGERS[number] }) {
  const [selectedIndex, setSelectedIndex] = useState(0)
  const [paused, setPaused] = useState<Readonly<Record<string, boolean>>>({})
  const selected = manager.branches[selectedIndex] ?? manager.branches[0]!

  return (
    <div className="grid overflow-hidden rounded-lg border border-border bg-background lg:grid-cols-[minmax(0,1fr)_22rem]">
      <div className="flex min-w-0 flex-col">
        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border bg-surface-subtle px-4 py-2.5">
          <div>
            <div className="flex items-center gap-2">
              <Workflow className="size-3.5 text-tone-blue" />
              <h2 className="text-sm font-semibold text-foreground">{manager.title}</h2>
              <span className="rounded-sm border border-border bg-background px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">Automation</span>
            </div>
            <p className="mt-0.5 text-xs text-muted-foreground">{manager.description}</p>
          </div>
          <span className="text-xs text-muted-foreground tabular-nums">{manager.branches.length} rules</span>
        </div>

        <div className="divide-y divide-border">
          {manager.branches.map((branch, idx) => {
            const active = idx === selectedIndex
            const isPaused = paused[branch.command] === true
            return (
              <div
                key={branch.command}
                onClick={() => setSelectedIndex(idx)}
                className={cn("cursor-pointer space-y-2.5 px-4 py-3.5 transition-colors hover:bg-surface-subtle", active && "border-s-2 border-s-data-accent bg-tone-blue/[0.05]", isPaused && "opacity-65")}
              >
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div className="flex flex-wrap items-center gap-2 text-xs">
                    <span className="rounded-sm bg-muted px-1.5 py-0.5 font-mono text-[10px] font-semibold uppercase text-muted-foreground">When</span>
                    <code className="font-semibold text-foreground">{branch.event}</code>
                    <ArrowRight className="size-3 text-muted-foreground" />
                    <span className="rounded-sm bg-muted px-1.5 py-0.5 font-mono text-[10px] font-semibold uppercase text-muted-foreground">If</span>
                    <span className="font-medium text-foreground">{branch.when}</span>
                    <ArrowRight className="size-3 text-muted-foreground" />
                    <span className="rounded-sm bg-tone-blue/10 px-1.5 py-0.5 font-mono text-[10px] font-semibold uppercase text-tone-blue">Then</span>
                    <code className="font-semibold text-tone-blue">{branch.command}</code>
                  </div>
                  <button
                    type="button"
                    aria-pressed={!isPaused}
                    onClick={(e) => { e.stopPropagation(); setPaused((c) => ({ ...c, [branch.command]: !c[branch.command] })) }}
                    className={cn("rounded-sm border px-2 py-0.5 text-[11px] font-medium", isPaused ? "border-border bg-surface-subtle text-muted-foreground" : "border-tone-green/30 bg-tone-green/10 text-tone-green")}
                  >
                    {isPaused ? "Paused (draft)" : "Active"}
                  </button>
                </div>
                <p className="text-[11px] text-muted-foreground">Reads: {branch.reads} · Target: {TYPES[branch.target].name} aggregate</p>
              </div>
            )
          })}
        </div>
      </div>

      <aside aria-label="Automation inspector" className="flex flex-col border-t border-border bg-surface-subtle/35 lg:border-s lg:border-t-0">
        <div className="border-b border-border bg-surface-subtle px-4 py-2.5">
          <p className="text-[10px] font-semibold tracking-wider uppercase text-muted-foreground">Automation Inspector</p>
          <code className="mt-0.5 block text-sm font-semibold text-tone-blue">{selected.command}</code>
        </div>
        <div className="flex-1 space-y-4 p-4 text-xs">
          <div className="space-y-1">
            <p className="text-[11px] font-medium text-muted-foreground">Trigger event</p>
            <Link to="/sources/$concept" params={{ concept: selected.aggregate }} search={{ tab: "events" }} hash={selected.event}
              className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1.5 font-mono font-medium text-foreground hover:border-data-accent hover:text-tone-blue">
              <GitBranch className="size-3 text-muted-foreground" />{selected.event}
            </Link>
          </div>
          <div className="space-y-1 border-t border-border pt-3">
            <p className="text-[11px] font-medium text-muted-foreground">Condition &amp; context</p>
            <p className="font-medium text-foreground">{selected.when}</p>
            <p className="text-muted-foreground">{selected.reads}</p>
          </div>
          <div className="space-y-1.5 border-t border-border pt-3">
            <p className="text-[11px] font-medium text-muted-foreground">Dispatches command</p>
            <Link to="/sources/$concept" params={{ concept: COMMANDS[selected.command].on }} search={{ tab: "commands" }} hash={selected.command}
              className="flex items-center justify-between rounded-md border border-border bg-background px-2.5 py-2 font-mono font-medium text-tone-blue hover:border-data-accent">
              <span>{selected.command}</span><ArrowRight className="size-3" />
            </Link>
            <Link to="/guarantees" hash={selected.rule}
              className="flex items-center justify-between rounded-md border border-border bg-background px-2.5 py-2 font-medium text-foreground hover:border-data-accent hover:text-tone-blue">
              <span className="inline-flex items-center gap-1.5"><ShieldCheck className="size-3.5 text-tone-blue" />Open guarding invariant</span>
              <ArrowRight className="size-3 text-muted-foreground" />
            </Link>
          </div>
          <div className="border-t border-border pt-3">
            <p className="mb-1.5 text-[11px] font-medium text-muted-foreground">Backing systems</p>
            <SystemDependencies systems={selected.systems} needs={selected.reads} compact />
          </div>
        </div>
      </aside>
    </div>
  )
}

export function FinanceWorkflows() {
  const { process } = useParams({ strict: false })
  const manager = PROCESS_MANAGERS.find((item) => item.id === process)
  const machine = MACHINES.find((item) => item.id === process) ?? MACHINES[0]
  return (
    <section className="space-y-3">
      <FinanceHeading
        title="Workflows & Automations"
        description="Branching object lifecycles and event-driven automations — click any transition to inspect guards or tune parameters."
        actions={
          <nav aria-label="Finance workflows" className="inline-flex flex-wrap items-center gap-1 rounded-md border border-border bg-surface-subtle p-0.5 text-xs">
            {[...MACHINES, ...PROCESS_MANAGERS].map((item) => {
              const active = item.id === (manager?.id ?? machine.id)
              return (
                <Link
                  key={item.id}
                  to="/workflows/$process"
                  params={{ process: item.id }}
                  aria-current={active ? "page" : undefined}
                  className={cn(
                    "rounded-sm px-2.5 py-1 font-medium transition-colors",
                    active ? "bg-background text-foreground shadow-xs ring-1 ring-border" : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  {"type" in item ? TYPES[item.type].name : item.title}
                </Link>
              )
            })}
          </nav>
        }
      />
      {manager ? <ProcessManager key={manager.id} manager={manager} /> : <FinanceStateMachine key={machine.id} machine={machine} />}
    </section>
  )
}
