//! The trusted-base gate set: directories and files pinned to the base.

/// Directories pinned to the trusted base, besides every workflow: the set of tracked files
/// under each, and every such file's type, mode and bytes. The guard crate is one: it is the
/// trusted lane's own source (rule, wiring, manifest, tests), so no file in it, present or
/// added, changes without `/approve-gates`. `.cargo/` is the other: Cargo reads its config
/// from the working directory and every parent whatever `--manifest-path` says, so a
/// `linker`, `rustc-wrapper` or `[env]` entry there would apply to the trusted build.
/// [`super::gate_source_errors`] keeps the guard's sources from reaching outside.
pub(super) const GATE_DIRECTORIES: [&str; 2] = [".cargo/", "crates/norn-skeleton-guard/"];

/// Gate files pinned to the trusted base, besides every workflow and [`GATE_DIRECTORIES`].
/// A gate file absent from the base must stay absent. The toolchain files are gates although
/// none exists, as defence in depth: rustup reads them from the working directory, but the
/// gated `validate-runner-policy.py` sets `RUSTUP_TOOLCHAIN`, which takes precedence.
pub(super) const GATE_FILES: [&str; 41] = [
    // Python isolation (#513): the base-owned check and its canaries, and every file a gated
    // Python file loads by explicit path, so none of them can change what a gate decides.
    "scripts/ci/python_isolation.py",
    "scripts/ci/test_python_isolation.py",
    "scripts/verification_attestation.py",
    "scripts/quint_apalache.py",
    "scripts/write-verification-result.py",
    "verification/coverage-v1/validate_result.py",
    "scripts/importers/erpnext_runtime.py",
    "verification/kernel-v2/validate.py",
    "rust-toolchain.toml",
    "rust-toolchain",
    "verification/coverage-v1/fixtures/pull-request-concurrency.json",
    "scripts/ci/core.sh",
    "scripts/ci/actions-secret-inventory.py",
    "scripts/ci/infisical-oidc.sh",
    "scripts/ci/secret-scan.sh",
    "scripts/ci/test_infisical_oidc.py",
    "scripts/ci/contract_diff.py",
    "scripts/ci/test_contract_diff.py",
    "scripts/ci/contract-breaks.json",
    "scripts/ci/rivetkit_channel.py",
    "scripts/ci/test_rivetkit_channel.py",
    "scripts/ci/implementation_receipts.py",
    ".config/nextest.toml",
    "scripts/ci/validate-runner-policy.py",
    "scripts/proof_enrollment.py",
    "verification/coverage-v1/validate.py",
    "verification/coverage-v1/negative_canaries.py",
    "scripts/coverage_manifest.py",
    "scripts/verify-promoted-subsystems.sh",
    "scripts/kernel_inline_verus.py",
    "scripts/kernel_verus_ir.py",
    "scripts/test_kernel_inline_verus.py",
    "scripts/verify-kernel-version-kani.sh",
    "scripts/verify-kernel-inline-verus.sh",
    "scripts/bootstrap-verification-tools.sh",
    "verification/coverage-v1/manifest/index.json",
    "verification/mutants/version_check/saturating_add.patch",
    "verification/mutants/version_check/zero.patch",
    "scripts/check-proof-holes.py",
    "crates/norn-skeleton-guard/baseline.json",
    "crates/norn-checker/tests/non_code_literals.baseline.json",
];
