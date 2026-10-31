/** @jsxImportSource react */
import { isRecord, stringifyExact } from "./json"
import { message } from "./messages"

const MAX_POINTS = 256
const TOKEN = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$/
interface Point { readonly x: number; readonly y: number; readonly xText: string; readonly yText: string }

function scalar(value: unknown): string | undefined {
  return typeof value === "string" && value.length <= 128 && TOKEN.test(value) ? value : undefined
}

function points(value: unknown): readonly Point[] | undefined {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_POINTS) return undefined
  const result: Point[] = []
  for (const pair of value) {
    if (!Array.isArray(pair) || pair.length !== 2) return undefined
    const xText = scalar(pair[0]), yText = scalar(pair[1])
    if (xText === undefined || yText === undefined) return undefined
    const x = Number(xText), y = Number(yText)
    if (!Number.isFinite(x) || !Number.isFinite(y)) return undefined
    result.push({ x, y, xText, yText })
  }
  return result
}

// Coordinates alone use binary64 approximations. Original server tokens remain
// visible in the data table; no statistic, threshold or publication state is computed.
function coordinates(values: readonly number[], start: number, length: number) {
  const magnitude = Math.max(1, ...values.map(Math.abs))
  const normalized = values.map(value => value / magnitude)
  const low = Math.min(...normalized), high = Math.max(...normalized)
  const span = high - low
  return normalized.map(value => span === 0 ? start + length / 2 : start + (value - low) / span * length)
}

function Plot({ value, title, axes, locale }: { value: unknown; title: string; axes: string; locale: string }) {
  const rows = points(value)
  if (!rows) return <p role="status" className="text-xs text-muted-foreground">{message(locale, "analysis.plotUnavailable", { title })}</p>
  const x = coordinates(rows.map(row => row.x), 30, 300)
  const y = coordinates(rows.map(row => row.y), 160, -135)
  return <figure className="m-0 min-w-0 rounded-md border border-border-subtle p-3">
    <figcaption className="text-xs font-medium text-foreground">{title}</figcaption>
    <svg role="img" aria-label={`${title}: ${axes}`} viewBox="0 0 360 190" className="mt-2 block w-full max-w-xl text-primary">
      <title>{title}</title>
      <desc>{axes}</desc>
      <path d="M30 20V165H340" fill="none" stroke="currentColor" strokeOpacity=".3" />
      {rows.map((row, index) => <circle key={index} cx={x[index]} cy={y[index]} r="2.5" fill="currentColor" fillOpacity=".7"><title>{`${row.xText}, ${row.yText}`}</title></circle>)}
    </svg>
    <p className="m-0 text-2xs text-muted-foreground">{axes}</p>
    <details className="mt-2 text-xs"><summary className="cursor-pointer text-muted-foreground">{message(locale, "analysis.plotData")}</summary>
      <div className="mt-2 max-h-48 overflow-auto"><table className="w-full text-start font-mono text-2xs"><caption className="sr-only">{title}</caption><thead><tr><th scope="col">x</th><th scope="col">y</th></tr></thead><tbody>{rows.map((row, index) => <tr key={index}><td dir="ltr">{row.xText}</td><td dir="ltr">{row.yText}</td></tr>)}</tbody></table></div>
    </details>
  </figure>
}

function text(value: unknown): string {
  return typeof value === "string" ? value : stringifyExact(value) ?? "null"
}

/** Display the cell's versioned report. Every estimate, obligation and state is
 * server supplied. This renderer neither admits reports nor grants publication. */
export function AnalysisReport({ value, locale }: { value: Record<string, unknown>; locale: string }) {
  if (value.format !== "norn.analysis.report/1" || value.numericEncoding !== "binary64-text/1" || !isRecord(value.result))
    return <code dir="ltr" className="break-all text-2xs">{stringifyExact(value)}</code>
  const report = value.result
  const diagnostics = Array.isArray(report.diagnostics) ? report.diagnostics.slice(0, 32).filter(isRecord) : []
  const obligations = Array.isArray(value.obligations) ? value.obligations.slice(0, 32).filter(isRecord) : []
  const fit = isRecord(report.fit) ? report.fit : undefined
  const plots = isRecord(report.plots) ? report.plots : fit && isRecord(fit.plots) ? fit.plots : undefined
  const predictions = Array.isArray(report.predictions) && report.predictions.length <= MAX_POINTS
    ? report.predictions.map(row => Array.isArray(row) && row.length === 4 ? [row[2], row[3]] : undefined) : undefined
  const cook = plots && Array.isArray(plots.cooksDistance) && plots.cooksDistance.length <= MAX_POINTS
    ? plots.cooksDistance.map((distance, index) => [String(index + 1), distance]) : undefined
  const estimates = ["n", "intercept", "slope", "difference", "pValue", "interval", "trainingN", "evaluationN", "rmse", "baselineRmse"]
    .filter(key => Object.hasOwn(report, key))
  return <div className="min-w-0 space-y-5 text-sm" data-analysis-report="1">
    <div className="flex flex-wrap items-baseline gap-x-4 gap-y-1 border-b border-border-subtle pb-2">
      <span className="text-2xs uppercase tracking-wider text-muted-foreground">{message(locale, "analysis.serverState")}</span>
      <strong dir="auto" className="font-mono text-sm">{text(value.state)}</strong>
      <span className="text-xs text-muted-foreground">{text(value.interpretation)}</span>
    </div>
    {typeof report.reason === "string" ? <p className="border-s-2 border-warning/50 ps-3 text-xs text-muted-foreground">{report.reason}</p> : null}
    {estimates.length ? <dl className="grid grid-cols-2 gap-x-6 gap-y-2 sm:grid-cols-3">{estimates.map(key => <div key={key} className="min-w-0"><dt className="text-2xs text-muted-foreground">{key}</dt><dd dir="ltr" className="m-0 break-all font-mono text-sm tabular-nums">{text(report[key])}</dd></div>)}</dl> : null}
    {obligations.length ? <div className="space-y-2"><h4 className="m-0 text-xs font-medium">{message(locale, "analysis.obligations")}</h4><ol className="m-0 grid list-none gap-3 p-0">{obligations.map((row, index) => <li key={index} className="grid gap-1 border-s border-border ps-3"><div className="flex flex-wrap gap-x-3 gap-y-1"><code className="text-2xs">{text(row.label)}</code><span className="text-xs">{text(row.id)}</span>{typeof row.passed === "boolean" ? <span className="text-2xs text-muted-foreground">{message(locale, "analysis.recordedOutcome", { value: String(row.passed) })}</span> : null}</div><p className="m-0 whitespace-pre-wrap break-words text-xs text-muted-foreground">{text(row.statement ?? row.detail ?? "")}</p>{typeof row.analyst === "string" ? <span className="text-2xs text-muted-foreground">{message(locale, "analysis.analyst", { analyst: row.analyst })}</span> : null}</li>)}</ol></div> : null}
    {diagnostics.length ? <div className="overflow-x-auto"><table className="w-full text-start text-xs"><caption className="mb-2 text-start font-medium">{message(locale, "analysis.diagnostics")}</caption><thead><tr><th scope="col" className="p-2 text-start">{message(locale, "analysis.check")}</th><th scope="col" className="p-2 text-start">{message(locale, "analysis.serverEvidence")}</th></tr></thead><tbody>{diagnostics.map((row, index) => <tr key={index} className="border-t border-border-subtle"><th scope="row" className="p-2 text-start font-normal">{text(row.method)}</th><td dir="ltr" className="max-w-lg break-all p-2 font-mono text-2xs">{stringifyExact(row)}</td></tr>)}</tbody></table></div> : null}
    {plots ? <div className="grid gap-4 lg:grid-cols-2"><Plot value={plots.residuals} title={message(locale, "analysis.residuals")} axes={message(locale, "analysis.residualAxes")} locale={locale} /><Plot value={plots.qq} title={message(locale, "analysis.qq")} axes={message(locale, "analysis.qqAxes")} locale={locale} /><Plot value={cook} title={message(locale, "analysis.influence")} axes={message(locale, "analysis.influenceAxes")} locale={locale} /></div> : null}
    {predictions ? <Plot value={predictions} title={message(locale, "analysis.predictions")} axes={message(locale, "analysis.predictionAxes")} locale={locale} /> : null}
    <p className="m-0 border-t border-border-subtle pt-2 text-2xs text-muted-foreground">{message(locale, "analysis.presentationOnly")}</p>
    <details className="text-xs"><summary className="cursor-pointer text-muted-foreground">{message(locale, "analysis.fullReport")}</summary><pre dir="ltr" className="max-h-96 overflow-auto whitespace-pre-wrap break-all font-mono text-2xs">{stringifyExact(value)}</pre></details>
  </div>
}
