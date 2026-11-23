import { Link, useParams } from "@tanstack/react-router"
import { Background, BaseEdge, Controls, EdgeLabelRenderer, Handle, MarkerType, Position, ReactFlow, getSmoothStepPath, type Edge, type EdgeProps, type Node, type NodeProps } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { ArrowRight, GitBranch, RotateCcw, ShieldCheck, SlidersHorizontal, Workflow } from "lucide-react"
import { useState } from "react"
import { type Tone } from "@/components/chip"
import { PageHeader, Panel, Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { cn } from "@/lib/utils"
import type { MenuEntry } from "../shell.tsx"
import { COMMANDS, MACHINES, PROCESS_MANAGERS, type FinanceMachine, type State, type StatePhase, type StateStep } from "./behavior.ts"
import { TYPES } from "./data.ts"
import { RULES } from "./operations.tsx"
import { SystemDependencies, TypeTile } from "./workspace.tsx"

type Direction = "forward" | "down" | "up" | "return-down" | "return-up"

type StateNode = Node<State, "state">
type TransitionEdge = Edge<{
  readonly step: StateStep
  readonly direction: Direction
  readonly lift: number
  readonly selected: boolean
  readonly choose: (step: StateStep) => void
}, "transition">

const STATE_TONE: Record<State["kind"], Tone> = { initial: "green", active: "gray", review: "amber", terminal: "gray", refused: "red" }
const STATE_KIND: Record<State["kind"], string> = { initial: "Start", active: "State", review: "Review", terminal: "End", refused: "Refused" }
const HANDLE = "!opacity-0 !border-0 !bg-transparent"

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
    <div className="canvas-node flex h-11 w-44 items-center gap-2 rounded-md border bg-card px-2.5 text-foreground">
      <Handle type="target" position={Position.Left} id="in" className={HANDLE} />
      <Handle type="source" position={Position.Right} id="out" className={HANDLE} />
      <Handle type="target" position={Position.Bottom} id="bottom-in" className={HANDLE} />
      <Handle type="source" position={Position.Bottom} id="bottom-out" className={HANDLE} />
      <Handle type="target" position={Position.Top} id="top-in" className={HANDLE} />
      <Handle type="source" position={Position.Top} id="top-out" className={HANDLE} />
      <span aria-label={STATE_KIND[data.kind]} className="size-2 shrink-0 rounded-full" style={{ background: `var(--tone-${STATE_TONE[data.kind]})` }} />
      <span className="text-[16px] leading-tight font-medium">{data.name}</span>
    </div>
  )
}

function TransitionView({ id, sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition, markerEnd, data }: EdgeProps<TransitionEdge>) {
  if (!data) return null
  const { step, direction, lift, selected, choose } = data
  const reverse = direction === "return-down" || direction === "return-up"
  const vertical = direction === "up" || direction === "down"
  const upper = direction === "return-up"
  const sameRow = sourceY === targetY
  const [forward, x, y] = getSmoothStepPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition, borderRadius: 8, offset: 12 })
  const turnY = upper ? Math.min(sourceY, targetY) - lift : Math.max(sourceY, targetY) + lift
  const d = reverse
    ? `M ${sourceX} ${sourceY} C ${sourceX} ${turnY}, ${sourceX} ${turnY}, ${sourceX - 18} ${turnY} H ${targetX + 18} C ${targetX} ${turnY}, ${targetX} ${turnY}, ${targetX} ${targetY}`
    : lift > 0 ? `M ${sourceX} ${sourceY} C ${sourceX + 55} ${sourceY + lift}, ${targetX - 55} ${targetY + lift}, ${targetX} ${targetY}` : forward
  const labelX = reverse ? (sourceX + targetX) / 2 : vertical ? x + (x < 210 ? 58 : -58) : lift > 0 && !sameRow ? x + 46 : x
  const labelY = reverse ? turnY + (upper ? -14 : 14)
    : vertical ? y
      : lift > 0 ? y + (sameRow ? lift * 0.75 : 28)
        : y - (sameRow || Math.abs(sourceY - targetY) > 190 ? 34 : 14)

  return (
    <>
      <BaseEdge
        id={id}
        path={d}
        markerEnd={markerEnd}
        style={{ stroke: selected ? "var(--foreground)" : "var(--edge)", strokeWidth: selected ? 2 : 1.35, strokeDasharray: reverse ? "4 3" : undefined }}
      />
      <EdgeLabelRenderer>
        <button
          type="button"
          aria-pressed={selected}
          onClick={() => choose(step)}
          title={`${step.command} · ${step.guard}`}
          className={cn(
            "nodrag nopan pointer-events-auto absolute z-10 rounded-sm border px-1.5 py-0.5 font-mono text-[15px] leading-tight font-medium whitespace-nowrap transition-colors",
            selected
              ? "border-foreground bg-foreground text-background"
              : "border-border bg-card text-foreground/85 hover:border-foreground/45",
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
    const bypassesState = source.row === target.row && Object.values(phase.states).some((state) =>
      state.row === source.row && state.column > source.column && state.column < target.column,
    )
    const lift = direction === "return-up" ? (step.command === "activatePlan" ? 36 : 74)
      : direction === "return-down" ? 34 + (index % 3) * 18
        : (bypassesState ? 112 : 0) + parallel * 44
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
      markerEnd: { type: MarkerType.ArrowClosed, color: step === selected ? "var(--foreground)" : "var(--edge)", width: 13, height: 13 },
    }]
  })
  const lastRow = Math.max(...Object.values(phase.states).map((state) => state.row))
  const returnAtBottom = edges.some(({ data }) => data?.direction === "return-down" && phase.states[data.step.from]?.row === lastRow)
  return (
    <div aria-label={`${phase.title} state graph`} className="h-[clamp(26rem,52svh,36rem)] w-full bg-canvas-surface">
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
    <Well>
      <BranchGraph phase={phase} selected={selected} onSelect={(step) => { setSelected(step); setInspectorTab("inspector") }} />

      <aside aria-label="Transition inspector" className="border-t bg-surface-subtle/40">
        <div className="flex items-center justify-between border-b px-3">
          <div role="tablist" className="flex gap-1">
            <button
              type="button"
              role="tab"
              aria-selected={inspectorTab === "inspector"}
              onClick={() => setInspectorTab("inspector")}
              className={cn("border-b-2 px-2.5 py-2.5 text-xs font-medium", inspectorTab === "inspector" ? "border-foreground text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}
            >
              Inspector
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={inspectorTab === "list"}
              onClick={() => setInspectorTab("list")}
              className={cn("border-b-2 px-2.5 py-2.5 text-xs font-medium", inspectorTab === "list" ? "border-foreground text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}
            >
              Transitions ({phase.steps.length})
            </button>
          </div>
          {draftCount > 0 && (
            <button type="button" onClick={() => setDrafts({})} className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
              <RotateCcw className="size-3" />Reset ({draftCount})
            </button>
          )}
        </div>

        {inspectorTab === "inspector" ? (
          <div className="divide-y px-4 text-meta">
            <div className="space-y-1 py-3.5">
              <p className="text-xs text-muted-foreground">Selected transition</p>
              <code className="block text-sm font-medium text-foreground">{selected.declared ?? selected.command}</code>
              <p className="text-xs text-muted-foreground">
                <span className="font-medium text-foreground">{phase.states[selected.from]?.name}</span> → <span className="font-medium text-foreground">{phase.states[selected.to]?.name}</span>
              </p>
            </div>

            <div className="space-y-1 py-3.5">
              <p className="text-xs text-muted-foreground">Guard condition</p>
              <p className="leading-relaxed text-foreground">{selected.guard}</p>
            </div>

            {params.length > 0 && (
              <div className="space-y-2.5 py-3.5">
                <div className="flex items-center justify-between">
                  <span className="inline-flex items-center gap-1.5 text-xs font-medium text-foreground">
                    <SlidersHorizontal className="size-3 text-muted-foreground" />Domain parameters
                  </span>
                  <span className="text-[11px] text-muted-foreground">Draft</span>
                </div>
                {params.map((param) => {
                  const k = `${selected.command}:${param.key}`
                  const val = drafts[k] ?? param.value
                  const changed = val !== param.value
                  return (
                    <label key={param.key} className="block space-y-1">
                      <span className="flex items-center justify-between text-xs text-muted-foreground">
                        <span>{param.label}</span>
                        {changed && <span className="font-medium text-tone-amber">Modified</span>}
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
                          className="h-7 bg-card font-mono text-xs"
                        />
                        <span className="shrink-0 text-xs text-muted-foreground">{param.unit}</span>
                      </div>
                    </label>
                  )
                })}
                {params.some((p) => drafts[`${selected.command}:${p.key}`] !== undefined && drafts[`${selected.command}:${p.key}`] !== p.value) && (
                  <p role="status" className="text-xs text-tone-amber">
                    Draft threshold active in preview — validated against invariants on publish.
                  </p>
                )}
              </div>
            )}

            {declaration && declaration.rules.length > 0 && (
              <div className="space-y-2 py-3.5">
                <p className="text-xs text-muted-foreground">Enforced invariants</p>
                <ul className="space-y-1.5">
                  {declaration.rules.map((rule) => (
                    <li key={rule}>
                      <Link to="/guarantees" hash={rule} className="inline-flex items-center gap-1.5 text-xs font-medium text-foreground hover:underline underline-offset-4">
                        <ShieldCheck className="size-3.5 text-muted-foreground" />
                        <span>{RULES.find((item) => item.id === rule)?.title ?? rule}</span>
                        <ArrowRight className="size-3 text-muted-foreground" />
                      </Link>
                    </li>
                  ))}
                  {selected.declared === "Payment.reclassify" && (
                    <li>
                      <Link to="/metrics" hash="corrections" className="inline-flex items-center gap-1.5 text-xs font-medium text-tone-amber hover:underline underline-offset-4">
                        <span>View reconciliation correction</span>
                        <ArrowRight className="size-3" />
                      </Link>
                    </li>
                  )}
                </ul>
              </div>
            )}

            <div className="py-3.5">
              <p className="mb-1.5 text-xs text-muted-foreground">Backing systems</p>
              <SystemDependencies systems={selected.systems} needs={selected.needs} />
            </div>
          </div>
        ) : (
          <ul className="divide-y">
            {phase.steps.map((step) => {
              const active = step === selected
              return (
                <li key={`${step.from}-${step.to}-${step.command}`}>
                  <button
                    type="button"
                    aria-pressed={active}
                    onClick={() => { setSelected(step); setInspectorTab("inspector") }}
                    className={cn("w-full px-3.5 py-2 text-start transition-colors hover:bg-muted/40", active && "border-s-2 border-s-foreground bg-muted/60")}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <code className="text-xs font-medium text-foreground">{step.command}</code>
                      <span className="truncate text-[11px] text-muted-foreground">{phase.states[step.from]?.name} → {phase.states[step.to]?.name}</span>
                    </div>
                    <p className="mt-0.5 truncate text-xs text-muted-foreground">{step.guard}</p>
                  </button>
                </li>
              )
            })}
          </ul>
        )}
      </aside>
    </Well>
  )
}

export function FinanceStateMachine({ machine }: { readonly machine: FinanceMachine }) {
  const [phaseName, setPhaseName] = useState(machine.phases[0].title)
  const phase = machine.phases.find((item) => item.title === phaseName) ?? machine.phases[0]
  return (
    <Panel
      label={
        <span className="inline-flex items-center gap-2">
          <TypeTile type={machine.type} />
          <span>{machine.title}</span>
          <code className="text-xs font-normal text-muted-foreground">{machine.property}</code>
        </span>
      }
      action={
        <div className="flex flex-wrap items-center gap-3">
          <div aria-label="State kinds" className="hidden items-center gap-3 text-xs text-muted-foreground md:flex">
            {(["initial", "review", "terminal", "refused"] as const).map((kind) => (
              <span key={kind} className="inline-flex items-center gap-1.5">
                <span className="size-1.5 rounded-full" style={{ background: `var(--tone-${STATE_TONE[kind]})` }} />
                {STATE_KIND[kind]}
              </span>
            ))}
          </div>
          {machine.phases.length > 1 && (
            <div role="group" aria-label="Lifecycle phase" className="inline-flex h-7 items-center rounded-md bg-muted p-0.5 text-xs">
              {machine.phases.map((item) => (
                <button
                  key={item.title}
                  type="button"
                  aria-pressed={item === phase}
                  onClick={() => setPhaseName(item.title)}
                  className="rounded-[0.3125rem] px-2.5 py-0.5 text-muted-foreground hover:text-foreground aria-pressed:bg-background aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-xs dark:aria-pressed:bg-accent"
                >
                  {item.title}
                </button>
              ))}
            </div>
          )}
        </div>
      }
    >
      <PhaseWorkbench key={phase.title} phase={phase} />
    </Panel>
  )
}

export const financeWorkflowMenu = (): readonly MenuEntry[] => [
  ...MACHINES.map(({ id, type }) => ({ href: `/workflows/${id}`, label: TYPES[type].name, group: "State machines" })),
  ...PROCESS_MANAGERS.map(({ id, title }) => ({ href: `/workflows/${id}`, label: title, group: "Automations" })),
]

function ProcessManager({ manager }: { readonly manager: typeof PROCESS_MANAGERS[number] }) {
  const [selectedIndex, setSelectedIndex] = useState(0)
  const [paused, setPaused] = useState<Readonly<Record<string, boolean>>>({})
  const selected = manager.branches[selectedIndex] ?? manager.branches[0]!

  return (
    <Panel icon={Workflow} label={manager.title} action={`${manager.branches.length} automations`}>
      <Well className="grid lg:grid-cols-[minmax(0,1fr)_22rem]">
        <div className="min-w-0 divide-y">
          <p className="px-4 py-2.5 text-xs text-muted-foreground">{manager.description}</p>
          {manager.branches.map((branch, idx) => {
            const active = idx === selectedIndex
            const isPaused = paused[branch.command] === true
            return (
              <div
                key={branch.command}
                onClick={() => setSelectedIndex(idx)}
                className={cn("cursor-pointer space-y-2 px-4 py-3.5 transition-colors hover:bg-muted/40", active && "border-s-2 border-s-foreground bg-muted/60", isPaused && "opacity-65")}
              >
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div className="flex flex-wrap items-center gap-2 text-meta">
                    <Badge variant="secondary" className="font-mono text-[10px] uppercase">When</Badge>
                    <code className="font-medium text-foreground">{branch.event}</code>
                    <ArrowRight className="size-3 text-muted-foreground" />
                    <Badge variant="secondary" className="font-mono text-[10px] uppercase">If</Badge>
                    <span className="text-foreground">{branch.when}</span>
                    <ArrowRight className="size-3 text-muted-foreground" />
                    <Badge variant="secondary" className="font-mono text-[10px] uppercase">Then</Badge>
                    <code className="font-medium text-foreground">{branch.command}</code>
                  </div>
                  <button
                    type="button"
                    aria-pressed={!isPaused}
                    onClick={(e) => { e.stopPropagation(); setPaused((c) => ({ ...c, [branch.command]: !c[branch.command] })) }}
                    className={cn("rounded-sm border px-2 py-0.5 text-xs font-medium", isPaused ? "border-border bg-muted/40 text-muted-foreground" : "border-tone-green/30 bg-tone-green/10 text-tone-green")}
                  >
                    {isPaused ? "Paused (draft)" : "Active"}
                  </button>
                </div>
                <p className="text-xs text-muted-foreground">Reads: {branch.reads} · Target: {TYPES[branch.target].name} aggregate</p>
              </div>
            )
          })}
        </div>

        <aside aria-label="Automation inspector" className="flex flex-col border-t bg-surface-subtle/40 lg:border-s lg:border-t-0">
          <div className="border-b px-4 py-3">
            <p className="text-xs text-muted-foreground">Automation inspector</p>
            <code className="mt-0.5 block text-sm font-medium text-foreground">{selected.command}</code>
          </div>
          <div className="flex-1 divide-y px-4 text-meta">
            <div className="space-y-1.5 py-3.5">
              <p className="text-xs text-muted-foreground">Trigger event</p>
              <Link to="/sources/$concept" params={{ concept: selected.aggregate }} search={{ tab: "events" }} hash={selected.event}
                className="inline-flex items-center gap-1.5 font-mono text-xs font-medium text-foreground hover:underline underline-offset-4">
                <GitBranch className="size-3 text-muted-foreground" />{selected.event}
              </Link>
            </div>
            <div className="space-y-1 py-3.5">
              <p className="text-xs text-muted-foreground">Condition &amp; context</p>
              <p className="font-medium text-foreground">{selected.when}</p>
              <p className="text-xs text-muted-foreground">{selected.reads}</p>
            </div>
            <div className="space-y-2 py-3.5 text-xs">
              <p className="text-muted-foreground">Dispatches command</p>
              <Link to="/sources/$concept" params={{ concept: COMMANDS[selected.command].on }} search={{ tab: "commands" }} hash={selected.command}
                className="flex items-center justify-between py-0.5 font-mono font-medium text-foreground hover:underline underline-offset-4">
                <span>{selected.command}</span><ArrowRight className="size-3 text-muted-foreground" />
              </Link>
              <Link to="/guarantees" hash={selected.rule}
                className="flex items-center justify-between py-0.5 font-medium text-foreground hover:underline underline-offset-4">
                <span className="inline-flex items-center gap-1.5"><ShieldCheck className="size-3.5 text-muted-foreground" />Open guarding invariant</span>
                <ArrowRight className="size-3 text-muted-foreground" />
              </Link>
            </div>
            <div className="py-3.5">
              <p className="mb-1.5 text-xs text-muted-foreground">Backing systems</p>
              <SystemDependencies systems={selected.systems} needs={selected.reads} compact />
            </div>
          </div>
        </aside>
      </Well>
    </Panel>
  )
}

export function FinanceWorkflows() {
  const { process } = useParams({ strict: false })
  const manager = PROCESS_MANAGERS.find((item) => item.id === process)
  const machine = MACHINES.find((item) => item.id === process) ?? MACHINES[0]
  return (
    <section className="space-y-6">
      <PageHeader
        title="Workflows"
        meta={`${MACHINES.length} lifecycles · ${PROCESS_MANAGERS.length} automations`}
        description="Branching object lifecycles and event-driven automations. Select any transition to inspect guards or tune parameters."
        actions={
          <nav aria-label="Finance workflows" className="inline-flex flex-wrap items-center gap-0.5 rounded-md bg-muted p-0.5 text-xs">
            {[...MACHINES, ...PROCESS_MANAGERS].map((item) => {
              const active = item.id === (manager?.id ?? machine.id)
              return (
                <Link
                  key={item.id}
                  to="/workflows/$process"
                  params={{ process: item.id }}
                  aria-current={active ? "page" : undefined}
                  className={cn(
                    "rounded-[0.3125rem] px-2.5 py-1 font-medium transition-colors",
                    active ? "bg-background text-foreground shadow-xs dark:bg-accent" : "text-muted-foreground hover:text-foreground",
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
