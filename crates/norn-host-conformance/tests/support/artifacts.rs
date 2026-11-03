//! ANALYSIS-PUBLISH-1: compile the actual policy with its native Function and unit pins.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    missing_docs,
    reason = "checked typed fixture preparation"
)]
use std::{path::PathBuf, sync::OnceLock};
pub(crate) fn artifacts() -> Vec<norn_ir::ArtifactEnvelope> {
    static FIXTURES: OnceLock<Vec<norn_ir::ArtifactEnvelope>> = OnceLock::new();
    FIXTURES.get_or_init(|| {
        let dir=tempfile::tempdir().expect("fixture directory");
        let root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let function=norn_checker::function::admit_function_manifest(&norn_metrics::host::manifest().expect("configured Function")).expect("admitted Function");
        let script=r#"
const [root, output, manifest] = process.argv.slice(1)
if (!root || !output || !manifest) throw new Error("fixture arguments required")
const { cp } = await import("node:fs/promises")
await cp(`${root}/registry/core/src`, `${output}/registry/core/src`, { recursive: true })
await Bun.write(`${output}/registry/core/src/shared/metrics-pin.ts`, `export const FORECAST_MANIFEST_DIGEST = ${JSON.stringify(manifest)}\n`)
const { compilePack } = await import(`${root}/tools/norn-ts/src/pack.ts`)
const core = compilePack(`${output}/registry/core/src`, { scope: {}, registry: "registry/core", root: output, modules: [`${output}/registry/core/src/analytics.ts`] })
if (!core.ok) throw new Error(JSON.stringify(core.failures))
const units = await Bun.file(`${root}/registry/core/artifacts/core.units.json`).json()
const { compileDefinitions } = await import(`${root}/tools/norn-ts/src/compiler/definitions.ts`)
const source = compileDefinitions(`${root}/crates/norn-host-conformance/tests/scrap.source.ts`, { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "crates/norn-host-conformance/tests/scrap.source.ts" })
if (!source.ok) throw new Error(JSON.stringify(source.diagnostics))
await Bun.write(`${output}/scrap-publication.json`, JSON.stringify([...core.artifacts, units, ...source.artifacts]))
"#;
        let result=std::process::Command::new("bun").arg("--eval").arg(script).arg(root).arg(dir.path()).arg(function.digest.0).output().expect("typed compiler");
        assert!(result.status.success(),"{}{}",String::from_utf8_lossy(&result.stdout),String::from_utf8_lossy(&result.stderr));
        serde_json::from_slice(&std::fs::read(dir.path().join("scrap-publication.json")).expect("compiled declarations")).expect("typed artifacts")
    }).clone()
}
