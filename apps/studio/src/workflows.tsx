import { useQuery } from "@tanstack/react-query"
import { useState } from "react"
import { Background, Handle, MarkerType, Position, ReactFlow, type Edge, type Node, type NodeProps } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Panel, Well } from "@/components/panel"
import { cn } from "@/lib/utils"
import { words } from "./lib/format.ts"
import { recordedTransport } from "./lib/transport.ts"
import { focusOf, guardedFields, workflows, type Focus, type Workflow } from "./lib/workflows.ts"
import type { MenuEntry } from "./shell.tsx"

const state = (name: string) => words(name)

type StateNode = Node<{ label: string; tone: string; onFocus: () => void }, "state">

/** A state with forward handles on its sides and handles below for steps that go back. */
function StateBox({ data }: NodeProps<StateNode>) {
  return (
    <button type="button" onClick={data.onFocus} className={cn("w-36 rounded-lg border-2 bg-background px-3 py-2 text-center text-sm transition-opacity duration-200 ease-spring", data.tone)}>
      <Handle type="target" position={Position.Left} id="in" className="!opacity-0" />
      <Handle type="source" position={Position.Right} id="out" className="!opacity-0" />
      <Handle type="target" position={Position.Bottom} id="back-in" className="!left-1/3 !opacity-0" />
      <Handle type="source" position={Position.Bottom} id="back-out" className="!left-2/3 !opacity-0" />
      {data.label}
    </button>
  )
}

const NODE_TYPES = { state: StateBox }

/** The lifecycle as a graph: the focused step bright, the steps beside it visible, the rest dimmed. */
function Chart({ workflow, focus, onFocus }: { readonly workflow: Workflow; readonly focus: Focus; readonly onFocus: (step: string) => void }) {
  const { lifecycle } = workflow
  const near = new Set([...focus.states, ...workflow.steps.filter(({ transition }) => focus.adjacent.has(transition.id)).flatMap(({ transition }) => [...transition.from, transition.to])])
  const nodes: StateNode[] = [...workflow.layout].map(([name, { column, row }]) => {
    const initial = lifecycle.initial.includes(name)
    const terminal = lifecycle.terminal?.includes(name) ?? false
    return {
      id: name,
      position: { x: column * 220, y: row * 110 },
      type: "state",
      data: {
        label: state(name),
        tone: cn(focus.states.has(name) ? "border-primary font-medium" : initial ? "border-foreground/40" : terminal ? "border-muted-foreground bg-muted" : "border-border", !near.has(name) && "opacity-30"),
        onFocus: () => {
          const step = workflow.steps.find(({ transition }) => transition.from.includes(name)) ?? workflow.steps.find(({ transition }) => transition.to === name)
          if (step) onFocus(step.transition.id)
        },
      },
    }
  })
  const columnOf = (name: string) => workflow.layout.get(name)?.column ?? 0
  const edges: Edge[] = workflow.steps.flatMap(({ transition }) => transition.from.map((from) => {
    // A step back or sideways leaves and enters below the states instead of crossing them.
    const forward = columnOf(transition.to) > columnOf(from)
    const focused = transition.id === focus.step
    const visible = focused || focus.adjacent.has(transition.id)
    return {
      id: `${transition.id}:${from}`,
      source: from,
      target: transition.to,
      sourceHandle: forward ? "out" : "back-out",
      targetHandle: forward ? "in" : "back-in",
      type: forward ? "default" : "smoothstep",
      label: state(transition.id),
      markerEnd: { type: MarkerType.ArrowClosed, color: focused ? "var(--primary)" : undefined },
      labelBgPadding: [4, 2] as [number, number],
      labelStyle: { fontSize: 12, fontWeight: focused ? 600 : 400, cursor: "pointer" },
      style: { stroke: focused ? "var(--primary)" : undefined, strokeWidth: focused ? 2 : 1, opacity: visible ? 1 : 0.2, transition: "opacity 200ms" },
      zIndex: focused ? 1 : 0,
    }
  }))
  return (
    <div className="h-80 w-full">
      <ReactFlow nodes={nodes} edges={edges} nodeTypes={NODE_TYPES} onEdgeClick={(_, edge) => onFocus(edge.id.slice(0, edge.id.indexOf(":")))} fitView fitViewOptions={{ padding: 0.08, maxZoom: 1 }} nodesDraggable={false} nodesConnectable={false} elementsSelectable={false} proOptions={{ hideAttribution: true }}>
        <Background />
      </ReactFlow>
    </div>
  )
}

/** The step's declared description, else the fields its guard reads, with the declared rule one click away. */
function Condition({ transition }: { readonly transition: Workflow["steps"][number]["transition"] }) {
  const guards = transition.guards ?? []
  if (guards.length === 0) return <span className="text-muted-foreground">Always</span>
  const fields = guardedFields(guards)
  return (
    <div className="space-y-1">
      <div>{transition.description ?? (fields.length > 0 ? `Requires: ${fields.join(", ")}` : "A declared condition")}</div>
      <details className="text-meta text-muted-foreground">
        <summary className="cursor-pointer select-none">Declared rule</summary>
        <code className="block pt-1 font-mono text-xs break-words whitespace-normal">{guards.join(" and ")}</code>
      </details>
    </div>
  )
}

/** The focused step: what it changes, when it is allowed, what fires it and the laws that govern it. */
function StepCard({ workflow, step }: { readonly workflow: Workflow; readonly step: Workflow["steps"][number] }) {
  const { transition, actions } = step
  return (
    <div className="grid gap-4 p-4 text-sm md:grid-cols-[1fr_1fr_1.4fr] 2xl:grid-cols-1">
      <div className="md:col-span-3 2xl:col-span-1">
        <div className="text-panel font-medium">{state(transition.id)}</div>
        <div className="text-muted-foreground">{transition.from.map(state).join(" or ")} → {state(transition.to)}</div>
      </div>
      <div className="space-y-1">
        <div className="text-meta font-medium text-muted-foreground">Allowed only when</div>
        <Condition transition={transition} />
      </div>
      <div className="space-y-1">
        <div className="text-meta font-medium text-muted-foreground">Action</div>
        {actions.length === 0 && <div className="text-muted-foreground">None declared</div>}
        {actions.map((action) => (
          <div key={action.actionId} className="space-y-1">
            <Button size="sm" variant="outline" disabled>{action.title ?? words(action.actionId)}</Button>
            <div className="text-meta text-muted-foreground">Not available yet: the server does not accept actions here yet.</div>
          </div>
        ))}
      </div>
      <div className="space-y-1">
        <div className="text-meta font-medium text-muted-foreground">Laws that govern it</div>
        {workflow.laws.length === 0 && <div className="text-muted-foreground">None declared on {words(workflow.objectType)}</div>}
        {workflow.laws.map((law) => (
          <div key={law.id} className="space-y-1 rounded-md border bg-background p-3">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-mono text-xs">{law.id}</span>
              <Badge variant={law.severity === "error" ? "secondary" : "outline"}>{law.severity === "error" ? "Must hold" : "Warning"}</Badge>
            </div>
            <p className="text-meta text-muted-foreground">{law.rationale}</p>
          </div>
        ))}
      </div>
    </div>
  )
}

function WorkflowCard({ workflow }: { readonly workflow: Workflow }) {
  const { lifecycle } = workflow
  const [step, setStep] = useState(workflow.steps[0]?.transition.id ?? "")
  const focus = focusOf(workflow, step)
  const current = workflow.steps.find(({ transition }) => transition.id === step)
  return (
    <section id={anchor(workflow)} className="scroll-mt-4 space-y-3">
      <div className="space-y-1 px-1">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="text-section font-medium">{words(workflow.objectType)}: {words(lifecycle.property).toLowerCase()}</h2>
          <Badge variant="outline" title="Definitions the server admitted, replayed exactly. Not a live connection.">Recorded</Badge>
        </div>
        <p className="text-meta text-muted-foreground">
          {[workflow.description,
            `Starts as ${lifecycle.initial.map(state).join(" or ")}.`,
            lifecycle.terminal?.length ? `Ends as ${lifecycle.terminal.map(state).join(" or ")}.` : undefined,
            lifecycle.immutable?.length ? `Its details are locked while ${lifecycle.immutable.map(state).join(" or ")}.` : undefined,
          ].filter(Boolean).join(" ")}
        </p>
      </div>
      <div className="grid gap-3 2xl:grid-cols-[minmax(0,1fr)_20rem]">
        <Panel label="Steps" action="Select a step to focus it">
          <div className="flex flex-wrap gap-1.5 px-2.5 pb-2">
            {workflow.steps.map(({ transition }) => (
              <Button key={transition.id} size="sm" variant={transition.id === step ? "default" : "outline"} aria-pressed={transition.id === step} onClick={() => setStep(transition.id)}>
                {state(transition.id)}
              </Button>
            ))}
          </div>
          <Well><Chart workflow={workflow} focus={focus} onFocus={setStep} /></Well>
        </Panel>
        <Panel label="Focused step">{current ? <Well><StepCard workflow={workflow} step={current} /></Well> : null}</Panel>
      </div>
    </section>
  )
}

const anchor = (workflow: Workflow) => `workflow-${workflow.objectType}-${workflow.lifecycle.property}`

/** The admitted lifecycles, for the submenu island. */
export function useWorkflowsMenu(): readonly MenuEntry[] | undefined {
  const definitions = useQuery({ queryKey: ["definitions"], queryFn: recordedTransport.definitions })
  return definitions.data && workflows(definitions.data.map(({ document }) => document))
    .map((workflow) => ({ href: `#${anchor(workflow)}`, label: words(workflow.objectType) }))
}

export function WorkflowsPage() {
  const definitions = useQuery({ queryKey: ["definitions"], queryFn: recordedTransport.definitions })
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Workflows</h1>
        <p className="max-w-2xl text-muted-foreground">How each business process moves: its states, the steps between them, the condition each step requires, and the laws that govern it.</p>
      </div>
      {definitions.isPending && <p className="text-muted-foreground">Reading definitions…</p>}
      {definitions.isError && <p className="text-destructive">{definitions.error.message}</p>}
      {definitions.data && workflows(definitions.data.map(({ document }) => document)).map((workflow) => (
        <WorkflowCard key={`${workflow.objectType}:${workflow.lifecycle.property}`} workflow={workflow} />
      ))}
    </section>
  )
}
