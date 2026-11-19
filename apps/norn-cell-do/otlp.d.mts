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
export declare function isLoopback(hostname: string): boolean
export declare class Telemetry {
  constructor(config: ExportConfig, now?: () => number, send?: typeof fetch)
  send: typeof fetch
  count(name: string, attrs: Attributes, value?: number, unit?: string): void
  observe(name: string, attrs: Attributes, value: number, unit?: string, bounds?: readonly number[]): void
  span(name: string, attrs: Attributes, startMs: number, endMs: number, error?: boolean): void
  due(): boolean
  buffered(): boolean
  pump(): Promise<void>
  flush(send?: typeof fetch): Promise<void>
  log(message: string): void
}
export declare function telemetry(env: unknown, service: string): Telemetry | null
export declare function routeTable(templates: readonly string[]): (pathname: string) => string
export declare const CELL_ROUTES: readonly string[]
export declare function cellRoute(pathname: string): string
export declare function methodOf(method: string): string
export declare function statusClass(status: number): string
