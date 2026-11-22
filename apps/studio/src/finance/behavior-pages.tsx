import { Link, useParams } from "@tanstack/react-router"
import { Background, BaseEdge, Controls, EdgeLabelRenderer, Handle, MarkerType, Position, ReactFlow, getSmoothStepPath, type Edge, type EdgeProps, type Node, type NodeProps } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { ArrowRight, GitBranch, ShieldCheck, Workflow } from "lucide-react"
import { useState } from "react"
import { Chip, type Tone } from "@/components/chip"
import { Panel, Well } from "@/components/panel"
import { cn } from "@/lib/utils"
import type { MenuEntry } from "../shell.tsx"
import { COMMANDS, MACHINES, PROCESS_MANAGERS, type FinanceMachine, type State, type StatePhase, type StateStep } from "./behavior.ts"
import { TYPES } from "./data.ts"
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

function StateNodeView({ data }: NodeProps<StateNode>) {
  return <div className="canvas-node flex h-14 w-37 items-center gap-2 rounded-xl border border-node-border bg-card px-2 text-card-foreground">
    <Handle type="target" position={Position.Left} id="in" className={HANDLE} />
    <Handle type="source" position={Position.Right} id="out" className={HANDLE} />
    <Handle type="target" position={Position.Bottom} id="bottom-in" className={HANDLE} />
    <Handle type="source" position={Position.Bottom} id="bottom-out" className={HANDLE} />
    <Handle type="target" position={Position.Top} id="top-in" className={HANDLE} />
    <Handle type="source" position={Position.Top} id="top-out" className={HANDLE} />
    <span aria-label={STATE_KIND[data.kind]} className="size-2 shrink-0 rounded-full" style={{ background: `var(--tone-${STATE_TONE[data.kind]})` }} />
    <span className="text-[13px] leading-tight font-semibold">{data.name}</span>
  </div>
}

function TransitionView({ id, sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition, markerEnd, data }: EdgeProps<TransitionEdge>) {
  if (!data) return null
  const { step, direction, lift, selected, choose } = data
  const reverse = direction === "return-down" || direction === "return-up"
  const vertical = direction === "up" || direction === "down"
  const upper = direction === "return-up"
  const [forward, x, y] = getSmoothStepPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition, borderRadius: 10, offset: 10 })
  const turnY = upper ? Math.min(sourceY, targetY) - lift : Math.max(sourceY, targetY) + lift
  const d = reverse
    ? `M ${sourceX} ${sourceY} C ${sourceX} ${turnY}, ${sourceX} ${turnY}, ${sourceX - 16} ${turnY} H ${targetX + 16} C ${targetX} ${turnY}, ${targetX} ${turnY}, ${targetX} ${targetY}`
    : lift > 0 ? `M ${sourceX} ${sourceY} C ${sourceX + 60} ${sourceY + lift}, ${targetX - 60} ${targetY + lift}, ${targetX} ${targetY}` : forward
  const labelX = reverse ? (sourceX + targetX) / 2 : vertical ? x + (x < 185 ? 85 : -85) : lift > 0 ? x + 60 : x
  const labelY = reverse ? turnY + (upper ? -20 : 18) : vertical ? y : lift > 0 ? y + 40 : y - (sourceY === targetY || Math.abs(sourceY - targetY) > 200 ? 52 : 23)
  return <>
    <BaseEdge id={id} path={d} markerEnd={markerEnd}
      style={{ stroke: selected ? "var(--data-accent)" : "var(--edge)", strokeWidth: selected ? 2.6 : 1.5, strokeDasharray: reverse ? "5 3" : undefined }} />
    <EdgeLabelRenderer>
      <button type="button" aria-pressed={selected} onClick={() => choose(step)}
        title={`${step.command} · ${step.guard}`}
        className={cn("nodrag nopan pointer-events-auto absolute z-10 max-w-39 rounded-md border bg-card px-1.5 py-1 text-center shadow-sm hover:border-data-accent", selected && "border-data-accent bg-data-accent/10")}
        style={{ transform: `translate(-50%, -50%) translate(${labelX}px,${labelY}px)` }}>
        <span className="block font-mono text-[12px] font-semibold text-tone-blue">{step.command}</span>
        <span className="block text-[10px] leading-tight text-muted-foreground">{step.guard}</span>
      </button>
    </EdgeLabelRenderer>
  </>
}

const NODE_TYPES = { state: StateNodeView }
const EDGE_TYPES = { transition: TransitionView }

function BranchGraph({ phase, selected, onSelect }: { readonly phase: StatePhase; readonly selected: StateStep | undefined; readonly onSelect: (step: StateStep) => void }) {
  const nodes: StateNode[] = Object.entries(phase.states).map(([id, data]) => ({
    id, type: "state", data, position: { x: data.column * 182 + 76, y: data.row * 150 + 96 },
  }))
  const edges: TransitionEdge[] = phase.steps.flatMap((step, index) => {
    const source = phase.states[step.from]
    const target = phase.states[step.to]
    if (!source || !target) return []
    const direction: Direction = source.column < target.column ? "forward"
      : source.column === target.column ? source.row > target.row ? "up" : "down"
        : step.command === "activatePlan" || step.command === "keepPromise" ? "return-up" : "return-down"
    const parallel = phase.steps.slice(0, index).filter(previous => previous.from === step.from && previous.to === step.to).length
    const lift = direction === "return-up" ? step.command === "activatePlan" ? 29 : 77 : direction === "return-down" ? 30 + (index % 3) * 16 : parallel * 52
    const sourceHandle = direction === "forward" ? "out" : direction === "up" || direction === "return-up" ? "top-out" : "bottom-out"
    const targetHandle = direction === "forward" ? "in" : direction === "down" || direction === "return-up" ? "top-in" : "bottom-in"
    return [{
      id: `${step.from}-${step.to}-${step.command}`, source: step.from, target: step.to,
      sourceHandle, targetHandle,
      data: { step, direction, lift, selected: step === selected, choose: onSelect }, type: "transition",
      markerEnd: { type: MarkerType.ArrowClosed, color: "var(--data-accent)", width: 14, height: 14 },
    }]
  })
  const lastRow = Math.max(...Object.values(phase.states).map(state => state.row))
  const returnAtBottom = edges.some(({ data }) => data?.direction === "return-down" && phase.states[data.step.from]?.row === lastRow)
  return <div aria-label={`${phase.title} state graph`} className="h-[31rem] w-full rounded-xl border bg-canvas-surface">
    <ReactFlow nodes={nodes} edges={edges} nodeTypes={NODE_TYPES} edgeTypes={EDGE_TYPES} fitView fitViewOptions={{ padding: { top: "62px", bottom: returnAtBottom ? "82px" : "22px", left: "32px", right: "32px" }, maxZoom: 1.15 }}
      minZoom={0.35} maxZoom={2} nodesDraggable={false} nodesConnectable={false} nodesFocusable={false} edgesFocusable={false}
      zoomOnScroll={false} zoomOnDoubleClick={false} panOnScroll={false} preventScrolling={false} proOptions={{ hideAttribution: true }}>
      <Background />
      <Controls showInteractive={false} position="bottom-right" />
    </ReactFlow>
  </div>
}

function TransitionDetails({ step, phase }: { readonly step: StateStep; readonly phase: StatePhase }) {
  const declaration = step.declared ? COMMANDS[step.declared] : undefined
  return <div className="space-y-3 px-4 pt-1 pb-4">
    <div className="flex flex-wrap items-center gap-2 text-sm">
      <code className="font-semibold text-tone-blue">{step.declared ?? step.command}</code>
      <span className="text-muted-foreground">{phase.states[step.from]?.name} → {phase.states[step.to]?.name}</span>
    </div>
    <p className="text-sm"><span className="me-1 font-medium">Guard</span>{step.guard}</p>
    <SystemDependencies systems={step.systems} needs={step.needs} />
    {declaration && declaration.rules.length > 0 && <div className="flex flex-wrap gap-1.5">
      {declaration.rules.map(rule => <Link key={rule} to="/guarantees" hash={rule}><Chip tone="blue" icon={ShieldCheck}>{RULES.find(item => item.id === rule)?.title ?? rule}</Chip></Link>)}
      {step.declared === "Payment.reclassify" && <Link to="/metrics" hash="corrections"><Chip tone="amber" icon={ArrowRight}>View reconciliation correction</Chip></Link>}
    </div>}
  </div>
}

function PhaseView({ phase }: { readonly phase: StatePhase }) {
  const [selected, setSelected] = useState<StateStep | undefined>(phase.steps[0])
  return <div className="space-y-3">
    <BranchGraph phase={phase} selected={selected} onSelect={setSelected} />
    {selected && <Panel label="Selected transition" icon={GitBranch}><TransitionDetails step={selected} phase={phase} /></Panel>}
    <details className="rounded-xl border bg-card p-3">
      <summary className="cursor-pointer text-sm font-medium">All {phase.steps.length} transitions and guards</summary>
      <ul className="divide-y pt-2">{phase.steps.map(step => <li key={`${step.from}-${step.to}-${step.command}`} className="py-2">
        <button type="button" aria-pressed={selected === step} onClick={() => setSelected(step)} className="flex flex-wrap items-baseline gap-x-2 rounded px-1 text-start text-xs hover:bg-muted">
          <code className="font-semibold text-tone-blue">{step.command}</code>
          <span>{phase.states[step.from]?.name} → {phase.states[step.to]?.name}</span><span className="text-muted-foreground">· {step.guard}</span>
        </button>
      </li>)}</ul>
    </details>
  </div>
}

export function FinanceStateMachine({ machine }: { readonly machine: FinanceMachine }) {
  const [phaseName, setPhaseName] = useState(machine.phases[0].title)
  const phase = machine.phases.find(item => item.title === phaseName) ?? machine.phases[0]
  return <section className="space-y-3" aria-label={machine.title}>
    <div className="flex flex-wrap items-end justify-between gap-2 rounded-xl border bg-card px-3 py-2">
      <div className="flex items-center gap-2"><TypeTile type={machine.type} /><div><h2 className="text-sm font-semibold">{machine.title}</h2><code className="text-xs text-muted-foreground">{machine.property}</code></div></div>
      {machine.phases.length > 1 && <div role="group" aria-label="Lifecycle phase" className="flex gap-1">
        {machine.phases.map(item => <button key={item.title} type="button" aria-pressed={item === phase} onClick={() => setPhaseName(item.title)}
          className={cn("rounded-md px-2.5 py-1.5 text-xs font-medium text-muted-foreground hover:bg-muted", item === phase && "bg-data-accent/10 text-foreground")}>{item.title}</button>)}
      </div>}
    </div>
    <div aria-label="State kinds" className="flex flex-wrap gap-1.5">
      {(["initial", "review", "terminal", "refused"] as const).map(kind => <Chip key={kind} tone={STATE_TONE[kind]} dot>{STATE_KIND[kind]}</Chip>)}
    </div>
    <PhaseView key={phase.title} phase={phase} />
  </section>
}

export const financeWorkflowMenu = (): readonly MenuEntry[] => [
  ...MACHINES.map(({ id, type }) => ({ href: `/workflows/${id}`, label: TYPES[type].name, group: "State machines" })),
  ...PROCESS_MANAGERS.map(({ id, title }) => ({ href: `/workflows/${id}`, label: title, group: "Process managers" })),
]

function ProcessManager({ manager }: { readonly manager: typeof PROCESS_MANAGERS[number] }) {
  const events = [...new Set(manager.branches.map(({ event }) => event))]
  return <section className="space-y-3">
    <div className="rounded-xl border bg-card p-3">
      <div className="flex flex-wrap items-center gap-2"><Chip tone="violet" icon={Workflow}>Process manager</Chip><h2 className="text-section font-semibold">{manager.title}</h2></div>
      <p className="mt-1 text-sm text-muted-foreground">{manager.description}</p>
    </div>
    {events.map(event => {
      const reactions = manager.branches.filter(branch => branch.event === event)
      return <Panel key={event} label={event} icon={GitBranch} action={<Chip tone="violet">Incoming event</Chip>}>
        <Well className="space-y-2 px-4 pb-4">
          {reactions.map(({ aggregate, reads, when, command, rule, systems, target }) => <article key={command} className="space-y-2 rounded-xl border bg-surface-subtle p-3">
            <div className="grid items-center gap-2 md:grid-cols-[minmax(0,1fr)_auto_minmax(0,1.35fr)_auto_minmax(0,1.25fr)]">
              <Link to="/sources/$concept" params={{ concept: aggregate }} search={{ tab: "events" }} hash={event} className="flex items-center gap-2 text-xs font-medium text-tone-blue hover:underline"><TypeTile type={aggregate} />{TYPES[aggregate].name}</Link>
              <ArrowRight className="size-4 text-tone-blue" />
              <div className="text-xs"><strong className="block font-semibold">{when}</strong><span className="text-muted-foreground">Reads · {reads}</span></div>
              <ArrowRight className="size-4 text-tone-blue" />
              <Link to="/sources/$concept" params={{ concept: COMMANDS[command].on }} search={{ tab: "commands" }} hash={command} className="rounded-lg border bg-card px-2 py-1.5 text-xs text-tone-blue hover:border-data-accent">
                <code className="block break-all font-semibold">{command}</code><span className="text-muted-foreground">{TYPES[target].name} aggregate</span>
              </Link>
            </div>
            <div className="flex flex-wrap items-start justify-between gap-2 border-t pt-2">
              <SystemDependencies systems={systems} needs={reads} />
              <Link to="/guarantees" hash={rule}><Chip tone="blue" icon={ShieldCheck}>Open invariant</Chip></Link>
            </div>
          </article>)}
        </Well>
      </Panel>
    })}
  </section>
}

export function FinanceWorkflows() {
  const { process } = useParams({ strict: false })
  const manager = PROCESS_MANAGERS.find(item => item.id === process)
  const machine = MACHINES.find(item => item.id === process) ?? MACHINES[0]
  return <section className="space-y-4">
    <FinanceHeading title="Workflows" description="Follow each type's state machine and the process managers that coordinate commands across aggregates." />
    <nav aria-label="Finance workflows" className="space-y-1 rounded-xl border bg-card px-3 py-2">
      {([["State machines", MACHINES], ["Process managers", PROCESS_MANAGERS]] as const).map(([label, entries]) => {
        return <div key={label} className="flex flex-wrap items-center gap-1.5">
          <span className="min-w-28 text-xs font-medium text-muted-foreground">{label}</span>
          {entries.map(item => {
            const active = item.id === (manager?.id ?? machine.id)
            return <Link key={item.id} to="/workflows/$process" params={{ process: item.id }} aria-current={active ? "page" : undefined}
              className={cn("rounded-md px-2.5 py-1 text-xs font-medium hover:bg-muted", active && "bg-data-accent/10 text-tone-blue")}>
              {"type" in item ? TYPES[item.type].name : item.title}
            </Link>
          })}
        </div>
      })}
    </nav>
    {manager ? <ProcessManager manager={manager} /> : <FinanceStateMachine key={machine.id} machine={machine} />}
  </section>
}
