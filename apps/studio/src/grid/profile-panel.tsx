import { useQuery } from "@tanstack/react-query"
import { BarChart3 } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { EmptyState, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import type { Read } from "@/lib/exact"
import { NotServed } from "@/lib/live-transport"
import { profiled, readProfile, type ObjectProfile, type PropertyProfile } from "@/lib/profile"
import { atScale } from "@/lib/objects"

const percent = (part: number, whole: number) => whole === 0 ? "—" : `${((part / whole) * 100).toFixed(part === 0 || part === whole ? 0 : 1)}%`

/** A bin's bounds on one axis, for drawing only: numbers as they are, dates as days. */
const position = (value: unknown): number | undefined =>
  typeof value === "number" ? value : typeof value === "string" && !Number.isNaN(Date.parse(value)) ? Date.parse(value) / 86_400_000 : undefined

/**
 * How tall each bin draws. Equal-population bins hold the same count by construction, so their
 * height is density (count over the bin's width) and the distribution's shape shows; any other
 * histogram draws its counts. A bin narrower than a tenth of an even split draws at that width.
 * Drawing only: every figure shown is the cell's exact text.
 */
function heights(histogram: NonNullable<PropertyProfile["histogram"]>): readonly number[] {
  const counts = histogram.bins.map(({ count }) => count)
  if (histogram.kind !== "equal_population") return counts
  const lows = histogram.bins.map(({ lower }) => position(lower))
  const highs = histogram.bins.map(({ upper }) => position(upper))
  const first = lows[0]
  const last = highs.at(-1)
  if (first === undefined || last === undefined || lows.some((low) => low === undefined) || highs.some((high) => high === undefined) || last <= first) return counts
  const floor = (last - first) / (histogram.bins.length * 10)
  return histogram.bins.map(({ count }, index) => count / Math.max(floor, (highs[index] ?? 0) - (lows[index] ?? 0)))
}

/** A histogram as bars; each bar's bounds and count are the values the cell sent for it. */
function Bars({ read, at, histogram }: { readonly read: Read<ObjectProfile>; readonly at: string; readonly histogram: NonNullable<PropertyProfile["histogram"]> }) {
  const drawn = heights(histogram)
  const most = Math.max(Number.MIN_VALUE, ...drawn)
  return (
    <div className="flex h-8 w-40 items-end gap-px" role="img" aria-label={`${histogram.bins.length} bins, ${histogram.kind.replaceAll("_", " ")}`}>
      {histogram.bins.map((bin, index) => {
        const lower = profiled(read, `${at}/histogram/bins/${index}/lower`, bin.lower)
        const upper = profiled(read, `${at}/histogram/bins/${index}/upper`, bin.upper)
        return <div key={index} title={`${lower === upper ? lower : `${lower} to ${upper}`}: ${bin.count}`} className="min-w-1 flex-1 rounded-t-sm bg-chart-1/70" style={{ height: `${Math.max(4, ((drawn[index] ?? 0) / most) * 100)}%` }} />
      })}
    </div>
  )
}

/** The value at one quantile, as the cell sent it; `—` when it sent none at that percent. */
function quantile(read: Read<ObjectProfile>, at: string, property: PropertyProfile, percentile: number): string {
  const index = property.quantiles.findIndex(({ percent }) => percent === percentile)
  return index === -1 ? "—" : profiled(read, `${at}/quantiles/${index}/value`, property.quantiles[index]?.value)
}

/**
 * The cell's profile of one object type over the reader's authorized snapshot: per property its
 * count, nulls, distinct values, range, quartiles and histogram. Every figure is the cell's; rows
 * whose values it withholds from this reader are counted apart and add nothing to the figures.
 */
export function ProfilePanel({ objectType, scales = new Map() }: { readonly objectType: string; readonly scales?: ReadonlyMap<string, number> }) {
  const profile = useQuery({ queryKey: ["profile", objectType], queryFn: () => readProfile(objectType), retry: false })
  if (profile.isPending) return <p className="text-sm text-muted-foreground">Profiling the records…</p>
  if (profile.isError) return profile.error instanceof NotServed
    ? <EmptyState icon={BarChart3} title="Not served">{profile.error.message}</EmptyState>
    : <p role="alert" className="text-sm text-destructive">{profile.error.message}</p>
  const read = profile.data
  // A numeric figure is written at its property's declared scale, as the record shows it.
  const scaled = (property: PropertyProfile, text: string) => property.kind === "numeric" ? atScale(text, scales.get(property.property)) : text
  const { rows, withheldRows, quarantinedRows, approximate, scanLimit, engine, properties } = read.document
  return (
    <Panel icon={BarChart3} label="Profile" action={<span className="text-meta text-muted-foreground">computed by the {engine} engine</span>}>
      <div className="space-y-3 p-1">
        <Well className="flex flex-wrap items-baseline gap-x-6 gap-y-1 px-4 py-3 text-sm tabular-nums">
          <span><b className="font-medium">{rows}</b> records profiled</span>
          {withheldRows > 0 && <span><b className="font-medium">{withheldRows}</b> withheld from you, not counted</span>}
          <span><b className="font-medium">{quarantinedRows}</b> in quarantine</span>
          {approximate && <Badge variant="outline" title={`The cell profiles at most ${scanLimit} visible records; past that the figures cover only the first ${scanLimit}.`}>First {scanLimit} records</Badge>}
        </Well>
        {properties.length === 0
          ? <EmptyState icon={BarChart3}>This type declares no properties to profile.</EmptyState>
          : <TableCard><Table>
            <TableHeader><TableRow>
              <TableHead>Property</TableHead><TableHead className="text-right">Values</TableHead><TableHead className="text-right">Empty</TableHead><TableHead className="text-right">Distinct</TableHead>
              <TableHead className="text-right">Min</TableHead><TableHead className="text-right">Median</TableHead><TableHead className="text-right">Max</TableHead><TableHead>Distribution</TableHead>
            </TableRow></TableHeader>
            <TableBody>
              {properties.map((property, index) => {
                const at = `/properties/${index}`
                const ranged = property.kind === "numeric" || property.kind === "temporal"
                return (
                  <TableRow key={property.property}>
                    <TableCell><div className="font-medium">{property.property.replaceAll("_", " ")}</div><div className="text-xs text-muted-foreground">{property.kind}</div></TableCell>
                    <TableCell className="text-right tabular-nums">{property.count}</TableCell>
                    <TableCell className="text-right tabular-nums" title={`${property.nulls} of ${property.count + property.nulls}`}>{percent(property.nulls, property.count + property.nulls)}</TableCell>
                    <TableCell className="text-right tabular-nums">{property.distinct}</TableCell>
                    <TableCell className="text-right font-mono text-xs">{ranged ? scaled(property, profiled(read, `${at}/min`, property.min)) : ""}</TableCell>
                    <TableCell className="text-right font-mono text-xs" title={ranged ? property.quantiles.map(({ percent: p }, q) => `p${p} ${profiled(read, `${at}/quantiles/${q}/value`, property.quantiles[q]?.value)}`).join(" · ") : undefined}>{ranged ? scaled(property, quantile(read, at, property, 50)) : ""}</TableCell>
                    <TableCell className="text-right font-mono text-xs">{ranged ? scaled(property, profiled(read, `${at}/max`, property.max)) : ""}</TableCell>
                    <TableCell>{property.histogram && property.histogram.bins.length > 0 ? <Bars read={read} at={at} histogram={property.histogram} /> : <span className="text-xs text-muted-foreground">{ranged ? "—" : `${property.distinct} distinct`}</span>}</TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table></TableCard>}
      </div>
    </Panel>
  )
}
