// Types for otlp.mjs, for the TypeScript hosts that import it (Studio's Worker).
export declare const MS_BUCKETS: readonly number[]
export declare const COUNT_BUCKETS: readonly number[]
export type Attributes = Readonly<Record<string, string | number | boolean>>
export interface ExportConfig {
  readonly endpoint: string
  readonly headers: Readonly<Record<string, string>>
  readonly service: string
  readonly resource: ReadonlyArray<readonly [string, string]>
  readonly sampleRatio: number
  readonly intervalMs: number
}
export declare function exportConfig(env: unknown, service: string): ExportConfig | null
export declare class Telemetry {
  constructor(config: ExportConfig, now?: () => number)
  count(name: string, attrs: Attributes, value?: number, unit?: string): void
  observe(name: string, attrs: Attributes, value: number, unit?: string, bounds?: readonly number[]): void
  span(name: string, attrs: Attributes, startMs: number, endMs: number, error?: boolean): void
  due(): boolean
  flushIfDue(send?: typeof fetch): Promise<void>
  flush(send?: typeof fetch): Promise<void>
  log(message: string): void
}
export declare function telemetry(env: unknown, service: string): Telemetry | null
export declare function routeOf(pathname: string): string
export declare function refusalCode(text: string): string
export declare function statusClass(status: number): string
