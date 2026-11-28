//! Native Rust TypeScript authoring compiler and formatter embedding `tsrs`.
//!
//! Slice 1 establishes the foundational Ring-1 authoring surface:
//!
//! - [`Profile`]: embedded, structurally validated `norn-effect` profile tables
//!   and ambient `@norn/authoring` type declarations.
//! - [`BoundedVfs`] and [`VfsLimits`]: a symlink-free, read-only, quota-bounded
//!   in-memory [`tsrs_vfs::FS`] serving bundled `lib.*.d.ts` files, virtual
//!   `/@norn/authoring/index.d.ts`, and caller-supplied TypeScript sources.
//! - [`AuthoringProgram`] and [`SymbolIdentity`]: bounded `tsrs` compilation,
//!   syntactic and semantic diagnostic collection, and alias-following symbol
//!   identity resolution via `tsrs_checker`.
//! - [`format_source`]: faithful port of `tools/norn-ts/src/fmt.ts` upholding
//!   `FMT-SEMANTIC-1` (never executes authored source and never deletes unused
//!   imports).
//! - [`ProfileChecker`]: faithful Rust port of `tools/norn-ts/src/check.ts`
//!   enforcing the `norn-effect` positive grammar, symbol identity checks,
//!   import closure checks, and `NORN-FLOW-*` diagnostics.

mod checker;
mod error;
mod fmt;
mod profile;
mod program;
mod vfs;

pub use checker::{FileReport, Position, ProfileChecker, ProfileDiagnostic, SymbolUse};
pub use error::AuthoringError;
pub use fmt::{format_source, norn_format_settings};
pub use profile::{
    AdmittedSymbol, AllowedImports, DiagnosticDoc, INTRINSICS_DTS, PROFILE_JSON, PackagePin,
    Profile, ProfilePins, RejectedConstruct, RejectedModule, RejectedSymbol, SymbolContext,
    SymbolUseKind,
};
pub use program::{
    AuthoringProgram, DiagnosticCategory, DiagnosticSummary, SymbolIdentity,
    default_compiler_options,
};
pub use vfs::{
    BoundedVfs, INTRINSICS_VIRTUAL_PATH, PINNED_EFFECT_DIST_PREFIX, PINNED_EFFECT_VIRTUAL_ROOT,
    VfsLimits, normalize_virtual_path,
};
