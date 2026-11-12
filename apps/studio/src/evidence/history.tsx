import { useQuery } from "@tanstack/react-query"
import { History as HistoryIcon, PencilLine, Workflow } from "lucide-react"
import { EmptyState, Panel, Well } from "@/components/panel"
import { CopyId } from "@/components/copy-id"
import { systemName } from "../lib/catalog.ts"
import { CellRefusal, NotServed } from "../lib/live-transport.ts"
import { atScale } from "../lib/objects.ts"
import { useCatalog } from "../sources.tsx"
import { changedProperties, readHistory, versionValue, type ObjectVersion } from "./review.ts"

/** Who or what wrote one version, in a line. */
function Writer({ version }: { readonly version: ObjectVersion }) {
  const writer = version.writer
  if (writer.kind === "mapping") {
    return (
      <div className="flex flex-wrap items-center gap-x-1 text-meta text-muted-foreground">
        <Workflow aria-hidden className="size-3.5" />
        Imported from {systemName(writer.lineage.sourceRecordRef.system)} record <code>{writer.lineage.sourceRecordRef.record}</code> by <code>{writer.lineage.mappingPin.artifactId}</code>
      </div>
    )
  }
  if (writer.kind === "action") {
    const reason = writer.input.correction_reason ?? writer.input.reason
    const evidence = writer.input.correction_evidence ?? writer.input.evidence
    return (
      <div className="space-y-0.5 text-meta text-muted-foreground">
        <div className="flex flex-wrap items-center gap-x-1"><PencilLine aria-hidden className="size-3.5" /><code>{writer.actionId}</code> by <span className="font-medium text-foreground">{writer.actor}</span></div>
        {typeof reason === "string" && <div>Reason: <span className="text-foreground">{reason}</span></div>}
        {typeof evidence === "string" && <div>Evidence: <code>{evidence}</code></div>}
      </div>
    )
  }
  return <div className="text-meta text-muted-foreground">Written by a rule, an import or a migration.</div>
}

/**
 * Every version of one record, newest first: what changed, which commit wrote
 * it and what wrote it (an import with its source record, or a governed
 * action with its verified actor and reason). Old values stay readable.
 */
export function History({ objectType, id }: { readonly objectType: string; readonly id: string }) {
  const read = useQuery({ queryKey: ["history", objectType, id], queryFn: () => readHistory(objectType, id), retry: false })
  // Exact decimals are stored canonically (72.00 as 72): each is shown at its property's declared scale.
  const { catalog } = useCatalog()
  const scales = new Map((catalog?.byName.get(objectType)?.fields ?? []).flatMap(({ id: field, decimal }) => decimal?.scale === undefined ? [] : [[field, decimal.scale] as const]))
  if (read.isPending) return <Panel icon={HistoryIcon} label="History"><Well className="p-3 text-meta text-muted-foreground">Reading the history…</Well></Panel>
  if (read.isError) {
    const message = read.error instanceof NotServed ? "This cell does not serve record history yet." : read.error instanceof CellRefusal ? read.error.message : "The history could not be read."
    return <Panel icon={HistoryIcon} label="History"><Well className="p-3 text-meta text-muted-foreground">{message}</Well></Panel>
  }
  const history = read.data
  const { versions, truncated } = history.document
  return (
    <Panel icon={HistoryIcon} label="History" action={`${versions.length} version${versions.length === 1 ? "" : "s"}`}>
      <Well className="p-3">
        {versions.length === 0 && <EmptyState icon={HistoryIcon}>No version of this record is stored.</EmptyState>}
        <ol className="space-y-3">
          {versions.map((version, index) => {
            const changed = changedProperties(history, index)
            const first = index === versions.length - 1 && !truncated
            return (
              <li key={version.version} className="space-y-1.5 rounded-md border bg-card p-3">
                <div className="flex flex-wrap items-center gap-x-2 text-sm">
                  <span className="font-medium">Version {version.version}</span>
                  <span className="text-meta text-muted-foreground">commit {version.commit} · {new Date(version.logicalTime).toISOString()}</span>
                </div>
                <Writer version={version} />
                {!first && changed.length > 0 && (
                  <table className="w-full text-sm">
                    <tbody>
                      {changed.map((property) => (
                        <tr key={property}>
                          <td className="py-0.5 pe-3 text-muted-foreground">{property.replaceAll("_", " ")}</td>
                          <td className="py-0.5 pe-3 font-mono tabular-nums text-muted-foreground line-through">{atScale(versionValue(history, index + 1, property), scales.get(property))}</td>
                          <td className="py-0.5 font-mono tabular-nums">{atScale(versionValue(history, index, property), scales.get(property))}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                )}
                {version.writer.kind === "mapping" && version.writer.lineage.documentDigest && <div className="text-meta text-muted-foreground">Source document <CopyId value={version.writer.lineage.documentDigest} short={12} /></div>}
              </li>
            )
          })}
        </ol>
        {truncated && <p className="pt-2 text-meta text-muted-foreground">Older versions exist; these are the newest the cell lists.</p>}
      </Well>
    </Panel>
  )
}
