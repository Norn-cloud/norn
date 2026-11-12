import { Background, Controls, Handle, MarkerType, Position, ReactFlow, type Edge, type Node, type NodeProps } from "@xyflow/react"
import "@xyflow/react/dist/style.css"
import { useNavigate } from "@tanstack/react-router"
import { useMemo, type CSSProperties } from "react"
import type { Catalog, Concept } from "./lib/catalog.ts"

type TypeNode = Node<{ concept: Concept }, "type">

const WIDTH = 220
const GAP_X = 90
const GAP_Y = 30
const HEIGHT = 76

function TypeCard({ data: { concept } }: NodeProps<TypeNode>) {
  const systems = [...new Set(concept.feeds.map(({ system }) => system))]
  return (
    <div className="w-[220px] cursor-pointer rounded-xl border bg-card px-3 py-2 shadow-sm transition-shadow hover:shadow-md">
      <Handle type="target" position={Position.Left} className="!opacity-0" />
      <div className="truncate text-sm font-medium" lang={concept.titleLang}>{concept.title}</div>
      <div className="truncate text-xs text-muted-foreground">{concept.fields.length} facts · {concept.relationships.length} links{systems.length > 0 ? ` · ${systems.join(", ")}` : ""}</div>
      <Handle type="source" position={Position.Right} className="!opacity-0" />
    </div>
  )
}

const NODE_TYPES = { type: TypeCard }

/**
 * Lay the object types out left to right by the links they declare: a type sits one column
 * right of the farthest type linking to it (cycles stop at the first visit), and each column
 * stacks its types in name order.
 */
export function layout(concepts: readonly Concept[]): ReadonlyMap<string, { readonly x: number; readonly y: number }> {
  const names = new Set(concepts.map(({ name }) => name))
  const column = new Map<string, number>()
  const place = (name: string, depth: number, path: ReadonlySet<string>) => {
    if (path.has(name) || depth > names.size || (column.get(name) ?? -1) >= depth) return
    column.set(name, depth)
    const concept = concepts.find((candidate) => candidate.name === name)
    for (const { target } of concept?.relationships ?? []) if (names.has(target)) place(target, depth + 1, new Set([...path, name]))
  }
  const targeted = new Set(concepts.flatMap(({ relationships }) => relationships.map(({ target }) => target)))
  for (const { name } of concepts) if (!targeted.has(name)) place(name, 0, new Set())
  for (const { name } of concepts) if (!column.has(name)) place(name, 0, new Set())
  const rows = new Map<number, number>()
  const positions = new Map<string, { x: number; y: number }>()
  for (const { name } of [...concepts].sort((a, b) => a.title.localeCompare(b.title))) {
    const at = column.get(name) ?? 0
    const row = rows.get(at) ?? 0
    rows.set(at, row + 1)
    positions.set(name, { x: at * (WIDTH + GAP_X), y: row * (HEIGHT + GAP_Y) })
  }
  return positions
}

/** The business model as a graph: each object type, and each link it declares to another. */
export function ModelGraph({ catalog }: { readonly catalog: Catalog }) {
  const navigate = useNavigate()
  const types = useMemo(() => catalog.concepts.filter(({ definition }) => definition === "object type"), [catalog])
  const { nodes, edges } = useMemo(() => {
    const positions = layout(types)
    const nodes: TypeNode[] = types.map((concept) => ({ id: concept.name, type: "type", position: positions.get(concept.name) ?? { x: 0, y: 0 }, data: { concept } }))
    const edges: Edge[] = types.flatMap((concept) => concept.relationships.filter(({ target }) => positions.has(target)).map((relationship) => ({
      id: `${concept.name}.${relationship.id}`, source: concept.name, target: relationship.target,
      label: `${relationship.id.replaceAll("_", " ")} (${relationship.count})`, labelStyle: { fontSize: 11 }, markerEnd: { type: MarkerType.ArrowClosed },
    })))
    return { nodes, edges }
  }, [types])
  const rows = Math.max(1, ...nodes.map(({ position }) => position.y / (HEIGHT + GAP_Y) + 1))
  return (
    <div className="h-[min(70vh,max(320px,calc(var(--rows)*106px+80px)))] overflow-hidden rounded-md border bg-surface-subtle" style={{ "--rows": rows } as CSSProperties}>
      <ReactFlow nodes={nodes} edges={edges} nodeTypes={NODE_TYPES} fitView colorMode="system" minZoom={0.2}
        onNodeClick={(_, node) => void navigate({ to: "/sources/$concept", params: { concept: node.id } })}
        nodesDraggable={false} nodesConnectable={false} edgesFocusable={false} proOptions={{ hideAttribution: true }}>
        <Background />
        <Controls showInteractive={false} />
      </ReactFlow>
    </div>
  )
}
