// Types for the deploy-time checker package in worker/checker/ (see checker.ts).
declare module "*/norn_checker_wasm.js" {
  export function initSync(module: { module: WebAssembly.Module }): unknown
  export function checkArtifactSet(artifactsJson: string, contextJson: string): string
  export function admitQuestion(artifactsJson: string, metricsJson: string, questionJson: string): string
}
declare module "*.wasm" {
  const module: WebAssembly.Module
  export default module
}
