import { useQuery } from "@tanstack/react-query"
import { Background, Handle, MarkerType, Position, ReactFlow, type Edge, type Node, type NodeProps } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { words } from "./lib/format.ts"
import { recordedTransport } from "./lib/transport.ts"
import { workflows, type Workflow } from "./lib/workflows.ts"
import type { MenuEntry } from "./shell.tsx"

const state = (name: string) => words(name)

type StateNode = Node<{ label: string; tone: string }, "state">

/** A state with forward handles on its sides and handles below for steps that go back. */
function StateBox({ data }: NodeProps<StateNode>) {
  return (
    <div className={`w-36 rounded-lg border-2 bg-background px-3 py-2 text-center text-sm ${data.tone}`}>
      <Handle type="target" position={Position.Left} id="in" className="!opacity-0" />
      <Handle type="source" position={Position.Right} id="out" className="!opacity-0" />
      <Handle type="target" position={Position.Bottom} id="back-in" className="!left-1/3 !opacity-0" />
      <Handle type="source" position={Position.Bottom} id="back-out" className="!left-2/3 !opacity-0" />
      {data.label}
    </div>
  )
}

const NODE_TYPES = { state: StateBox }

function Chart({ workflow }: { readonly workflow: Workflow }) {
  const { lifecycle } = workflow
  const nodes: StateNode[] = [...workflow.layout].map(([name, { column, row }]) => {
    const initial = lifecycle.initial.includes(name)
    const terminal = lifecycle.terminal?.includes(name) ?? false
    return {
      id: name,
      position: { x: column * 220, y: row * 110 },
      type: "state",
      data: { label: state(name), tone: initial ? "border-primary" : terminal ? "border-muted-foreground bg-muted" : "border-border" },
    }
  })
  const columnOf = (name: string) => workflow.layout.get(name)?.column ?? 0
  const edges: Edge[] = workflow.steps.flatMap(({ transition }) => transition.from.map((from) => {
    // A step back or sideways leaves and enters below the states instead of crossing them.
    const forward = columnOf(transition.to) > columnOf(from)
    return {
      id: `${transition.id}:${from}`,
      source: from,
      target: transition.to,
      sourceHandle: forward ? "out" : "back-out",
      targetHandle: forward ? "in" : "back-in",
      type: forward ? "default" : "smoothstep",
      label: state(transition.id),
      markerEnd: { type: MarkerType.ArrowClosed },
      labelBgPadding: [4, 2] as [number, number],
    }
  }))
  return (
    <div className="h-80 w-full rounded-lg border bg-surface-subtle">
      <ReactFlow nodes={nodes} edges={edges} nodeTypes={NODE_TYPES} fitView fitViewOptions={{ padding: 0.15, maxZoom: 1 }} nodesDraggable={false} nodesConnectable={false} elementsSelectable={false} proOptions={{ hideAttribution: true }}>
        <Background />
      </ReactFlow>
    </div>
  )
}

function WorkflowCard({ workflow }: { readonly workflow: Workflow }) {
  const { lifecycle } = workflow
  return (
    <Card id={anchor(workflow)} className="scroll-mt-4">
      <CardHeader>
        <div className="flex flex-wrap items-center gap-2">
          <CardTitle className="text-section font-medium">{words(workflow.objectType)}: {words(lifecycle.property).toLowerCase()}</CardTitle>
          <Badge variant="outline" title="Definitions the server admitted, replayed exactly. Not a live connection.">Recorded</Badge>
        </div>
        <CardDescription>
          {[workflow.description,
            `Starts as ${lifecycle.initial.map(state).join(" or ")}.`,
            lifecycle.terminal?.length ? `Ends as ${lifecycle.terminal.map(state).join(" or ")}.` : undefined,
            lifecycle.immutable?.length ? `Its details are locked while ${lifecycle.immutable.map(state).join(" or ")}.` : undefined,
          ].filter(Boolean).join(" ")}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        <Chart workflow={workflow} />
        <TableCard><Table>
          <TableHeader><TableRow><TableHead>Step</TableHead><TableHead>From → to</TableHead><TableHead>Allowed only when (as declared)</TableHead><TableHead>Action</TableHead></TableRow></TableHeader>
          <TableBody>
            {workflow.steps.map(({ transition, actions }) => (
              <TableRow key={transition.id}>
                <TableCell className="font-medium">{state(transition.id)}</TableCell>
                <TableCell>{transition.from.map(state).join(", ")} → {state(transition.to)}</TableCell>
                <TableCell className="max-w-sm whitespace-normal font-mono text-xs">{transition.guards?.join(" and ") ?? "Always"}</TableCell>
                <TableCell className="space-y-1">
                  {actions.length === 0 && <span className="text-muted-foreground">None declared</span>}
                  {actions.map((action) => (
                    <div key={action.actionId} className="space-y-1">
                      <Button size="sm" variant="outline" disabled>{action.title ?? words(action.actionId)}</Button>
                      <div className="text-meta text-muted-foreground">Not available yet: the server does not accept actions here yet.</div>
                    </div>
                  ))}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table></TableCard>
        {workflow.laws.length > 0 && (
          <div className="space-y-2">
            <h3 className="text-panel font-medium">Laws that govern it</h3>
            <ul className="space-y-2 text-sm">
              {workflow.laws.map((law) => (
                <li key={law.id} className="rounded-lg border p-3">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-mono text-xs">{law.id}</span>
                    <Badge variant={law.severity === "error" ? "secondary" : "outline"}>{law.severity === "error" ? "Must hold" : "Warning"}</Badge>
                  </div>
                  <p className="pt-1 text-muted-foreground">{law.rationale}</p>
                </li>
              ))}
            </ul>
          </div>
        )}
        <p className="text-xs text-muted-foreground">Committed and refused actions will appear here once the server accepts actions.</p>
      </CardContent>
    </Card>
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
