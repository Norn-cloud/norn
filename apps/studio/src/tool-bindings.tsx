import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link2, Play, Wrench } from "lucide-react"
import { useState } from "react"
import { EmptyState, Panel, Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { ConnectorProblem } from "./connections.tsx"
import { api, type Binding, type InputMapping, type Inventory, type RunRole, type Tool } from "./lib/connectors.ts"
import { buildInput, formFields, parseField, type FormField } from "./lib/schema-form.ts"

/**
 * Tool bindings (#772): the organization's record that a gateway tool may run from Norn, with
 * fixed input values and the least role that may run it. A tool with no binding cannot run. The
 * server checks every mapping against the tool's live input schema and decides every verb; `manage`
 * is its answer, shown as given.
 */

const encode = encodeURIComponent
const show = (value: unknown) => (typeof value === "string" ? value : JSON.stringify(value))

function FieldInput({ field, value, onChange }: { readonly field: FormField; readonly value: string; readonly onChange: (next: string) => void }) {
  if (field.kind === "enum" || field.kind === "boolean") {
    return (
      <select aria-label={field.label} className="h-9 rounded-md border bg-background px-2" value={value} onChange={(e) => onChange(e.target.value)}>
        <option value="">—</option>
        {(field.kind === "boolean" ? ["true", "false"] : field.options).map((option) => <option key={option} value={option}>{option}</option>)}
      </select>
    )
  }
  if (field.kind === "json") return <Textarea aria-label={field.label} rows={2} className="font-mono text-xs" value={value} onChange={(e) => onChange(e.target.value)} />
  return <Input aria-label={field.label} type={field.kind === "date" ? "date" : field.kind === "datetime" ? "datetime-local" : "text"} value={value} onChange={(e) => onChange(e.target.value)} />
}

/** Add or edit: label, the least role that may run it, and which inputs it fixes to which values. */
function BindingEditor({ tool, binding, onDone }: { readonly tool: Tool; readonly binding?: Binding; readonly onDone: () => void }) {
  const client = useQueryClient()
  const fields = formFields(tool.inputSchema) ?? []
  const [label, setLabel] = useState(binding?.label ?? tool.name)
  const [runRole, setRunRole] = useState<RunRole>(binding?.runRole ?? "member")
  const [fixed, setFixed] = useState<Record<string, string>>(Object.fromEntries(Object.entries(binding?.inputMapping ?? {}).map(([name, { fixed: value }]) => [name, show(value)])))
  const [problem, setProblem] = useState<string | null>(null)
  const save = useMutation({
    mutationFn: (inputMapping: InputMapping) => binding === undefined
      ? api<{ binding: Binding }>("/bindings", { method: "POST", body: { app: tool.app, tool: tool.name, label, runRole, inputMapping } })
      : api<{ binding: Binding }>(`/bindings/${encode(binding.id)}`, { method: "PATCH", body: { label, runRole, inputMapping } }),
    onSuccess: async () => { await client.invalidateQueries({ queryKey: ["connectors"] }); onDone() },
  })
  const submit = () => {
    const mapping: Record<string, { fixed: unknown }> = {}
    for (const field of fields) {
      if (!(field.name in fixed)) continue
      const parsed = parseField({ ...field, required: true }, fixed[field.name] ?? "")
      if (!parsed.ok) return setProblem(parsed.error)
      mapping[field.name] = { fixed: parsed.value }
    }
    setProblem(null)
    save.mutate(mapping)
  }
  return (
    <form aria-label={binding === undefined ? `Bind ${tool.name}` : `Edit ${binding.label}`} className="space-y-3 border-t px-4 py-3" onSubmit={(e) => { e.preventDefault(); submit() }}>
      <label className="grid gap-1 text-sm"><span className="font-medium">Name</span><Input aria-label="Binding name" value={label} maxLength={120} onChange={(e) => setLabel(e.target.value)} /></label>
      <label className="grid gap-1 text-sm">
        <span className="font-medium">Who may run it</span>
        <select aria-label="Who may run it" className="h-9 rounded-md border bg-background px-2" value={runRole} onChange={(e) => setRunRole(e.target.value as RunRole)}>
          <option value="member">Members and admins</option>
          <option value="admin">Admins only</option>
        </select>
      </label>
      <fieldset className="space-y-2">
        <legend className="text-sm font-medium">Fixed inputs</legend>
        {fields.length === 0 && <p className="text-meta text-muted-foreground">This tool's input has no fields to fix.</p>}
        {fields.map((field) => (
          <div key={field.name} className="flex flex-wrap items-center gap-2 text-sm">
            <label className="flex w-48 items-center gap-2">
              <input type="checkbox" aria-label={`Fix ${field.name}`} checked={field.name in fixed} onChange={(e) => setFixed(e.target.checked ? { ...fixed, [field.name]: "" } : Object.fromEntries(Object.entries(fixed).filter(([name]) => name !== field.name)))} />
              <span className="font-mono">{field.name}</span><span className="text-meta text-muted-foreground">{field.kind}</span>
            </label>
            {field.name in fixed && <FieldInput field={field} value={fixed[field.name] ?? ""} onChange={(next) => setFixed({ ...fixed, [field.name]: next })} />}
          </div>
        ))}
      </fieldset>
      <div className="flex gap-2">
        <Button size="sm" type="submit" disabled={save.isPending}>{binding === undefined ? "Add binding" : "Save binding"}</Button>
        <Button size="sm" variant="ghost" type="button" onClick={onDone}>Cancel</Button>
      </div>
      {problem && <p role="alert" className="text-sm text-destructive">{problem}</p>}
      {save.isError && <ConnectorProblem error={save.error} />}
    </form>
  )
}

/** A test run: the fields the binding leaves open, as a form from the tool's input schema. */
function TestRun({ binding, tool }: { readonly binding: Binding; readonly tool: Tool | undefined }) {
  const fields = (formFields(tool?.inputSchema) ?? []).filter((field) => !(field.name in binding.inputMapping))
  const [texts, setTexts] = useState<Record<string, string>>({})
  const [problem, setProblem] = useState<string | null>(null)
  const run = useMutation({ mutationFn: (input: unknown) => api<{ result: unknown }>(`/bindings/${encode(binding.id)}/run`, { method: "POST", body: { input } }) })
  const submit = () => {
    const built = buildInput(fields, texts)
    if (!built.ok) return setProblem(built.error)
    setProblem(null)
    run.mutate(built.value)
  }
  return (
    <form aria-label={`Test run ${binding.label}`} className="space-y-3 border-t px-4 py-3" onSubmit={(e) => { e.preventDefault(); submit() }}>
      {fields.map((field) => (
        <label key={field.name} className="grid gap-1 text-sm">
          <span className="font-medium">{field.label}{field.required && <span className="text-destructive"> *</span>} <span className="text-meta text-muted-foreground">{field.kind}</span></span>
          <FieldInput field={field} value={texts[field.name] ?? ""} onChange={(next) => setTexts({ ...texts, [field.name]: next })} />
          {field.description && <span className="text-meta text-muted-foreground">{field.description}</span>}
        </label>
      ))}
      <Button size="sm" type="submit" disabled={run.isPending}><Play />Run</Button>
      {problem && <p role="alert" className="text-sm text-destructive">{problem}</p>}
      {run.isError && <ConnectorProblem error={run.error} />}
      {run.isSuccess && <pre aria-label="Result" className="max-h-80 overflow-auto rounded-md bg-muted p-3 text-xs">{JSON.stringify(run.data.result, null, 2)}</pre>}
    </form>
  )
}

const useTools = (app: string | null) => useQuery({ queryKey: ["connectors", "tools", app], queryFn: () => api<{ tools: readonly Tool[] }>(`/apps/${encode(app ?? "")}/tools`), enabled: app !== null })

function BindingRow({ binding, manage }: { readonly binding: Binding; readonly manage: boolean }) {
  const client = useQueryClient()
  const [mode, setMode] = useState<"view" | "edit" | "run" | null>(null)
  const tools = useTools(mode === null ? null : binding.app)
  const tool = tools.data?.tools.find((entry) => entry.name === binding.tool)
  const remove = useMutation({ mutationFn: () => api(`/bindings/${encode(binding.id)}`, { method: "DELETE" }), onSuccess: () => client.invalidateQueries({ queryKey: ["connectors"] }) })
  const toggle = (next: "view" | "edit" | "run") => setMode(mode === next ? null : next)
  return (
    <div className="border-t first:border-t-0" data-binding={binding.id}>
      <div className="flex flex-wrap items-center gap-2 px-4 py-3">
        <div className="min-w-0 flex-1">
          <div className="font-medium">{binding.label}</div>
          <div className="font-mono text-meta text-muted-foreground">{binding.app}.{binding.tool}</div>
        </div>
        <Badge variant="outline">{binding.runRole === "admin" ? "Admins only" : "Members"}</Badge>
        <Button size="sm" variant="ghost" aria-expanded={mode === "view"} onClick={() => toggle("view")}>View</Button>
        <Button size="sm" variant="outline" onClick={() => toggle("run")}>Test run</Button>
        {manage && <Button size="sm" variant="outline" onClick={() => toggle("edit")}>Edit</Button>}
        {manage && <Button size="sm" variant="outline" disabled={remove.isPending} onClick={() => { if (window.confirm(`Remove ${binding.label}? It can no longer run from Norn.`)) remove.mutate() }}>Remove</Button>}
      </div>
      {mode === "view" && (
        <dl className="grid grid-cols-[8rem_1fr] gap-x-3 gap-y-1 px-4 pb-3 text-meta">
          <dt className="text-muted-foreground">Fixed inputs</dt>
          <dd>{Object.keys(binding.inputMapping).length === 0 ? "none" : Object.entries(binding.inputMapping).map(([name, { fixed }]) => `${name} = ${show(fixed)}`).join(", ")}</dd>
          <dt className="text-muted-foreground">Last changed</dt><dd>{new Date(binding.updatedAt).toLocaleString()}</dd>
          <dt className="text-muted-foreground">Input schema</dt>
          <dd><pre className="max-h-48 overflow-auto rounded-md bg-muted p-2 text-xs">{tools.isPending ? "Loading…" : JSON.stringify(tool?.inputSchema ?? null, null, 2)}</pre></dd>
        </dl>
      )}
      {mode !== null && mode !== "view" && tools.isError && <ConnectorProblem error={tools.error} />}
      {mode === "edit" && tool && <BindingEditor tool={tool} binding={binding} onDone={() => setMode(null)} />}
      {mode === "run" && !tools.isPending && !tools.isError && <TestRun binding={binding} tool={tool} />}
      {remove.isError && <ConnectorProblem error={remove.error} />}
    </div>
  )
}

/** Add: pick an app, then a tool, then bind it. */
function AddBinding() {
  const inventory = useQuery({ queryKey: ["connectors", "inventory"], queryFn: () => api<Inventory>("/inventory"), retry: false })
  const [app, setApp] = useState<string | null>(null)
  const [binding, setBinding] = useState<string | null>(null)
  const tools = useTools(app)
  if (inventory.isError) return <ConnectorProblem error={inventory.error} />
  return (
    <div className="space-y-2 border-t px-4 py-3">
      <label className="flex items-center gap-2 text-sm">
        <span className="font-medium">Add a binding for</span>
        <select aria-label="App" className="h-9 rounded-md border bg-background px-2" value={app ?? ""} onChange={(e) => { setApp(e.target.value || null); setBinding(null) }}>
          <option value="">choose an app</option>
          {(inventory.data?.apps ?? []).map((entry) => <option key={entry.id} value={entry.id}>{entry.name}</option>)}
        </select>
      </label>
      {tools.isError && <ConnectorProblem error={tools.error} />}
      {tools.data?.tools.map((tool) => (
        <div key={tool.name} className="rounded-md border">
          <div className="flex items-center gap-2 px-3 py-2">
            <div className="min-w-0 flex-1"><span className="font-mono text-sm">{tool.name}</span>{tool.description && <p className="line-clamp-1 text-meta text-muted-foreground">{tool.description}</p>}</div>
            {tool.bindings.length > 0 && <Badge variant="secondary">Bound</Badge>}
            <Button size="sm" variant="outline" aria-label={`Bind ${tool.name}`} onClick={() => setBinding(binding === tool.name ? null : tool.name)}>Bind</Button>
          </div>
          {binding === tool.name && <BindingEditor tool={tool} onDone={() => setBinding(null)} />}
        </div>
      ))}
    </div>
  )
}

export function ToolBindings() {
  const bindings = useQuery({ queryKey: ["connectors", "bindings"], queryFn: () => api<{ bindings: readonly Binding[]; manage: boolean }>("/bindings") })
  return (
    <Panel icon={Link2} label="Tool bindings">
      <Well>
        {bindings.isPending ? <p className="px-4 py-3 text-sm text-muted-foreground">Loading…</p>
          : bindings.isError ? <ConnectorProblem error={bindings.error} />
          : (
            <>
              {bindings.data.bindings.length === 0
                ? <EmptyState icon={Wrench}>No tool can run from Norn until an admin binds it.</EmptyState>
                : bindings.data.bindings.map((binding) => <BindingRow key={binding.id} binding={binding} manage={bindings.data.manage} />)}
              {bindings.data.manage && <AddBinding />}
            </>
          )}
      </Well>
    </Panel>
  )
}
