use norn_authoring::{
    AuthoringError, AuthoringProgram, BoundedVfs, DiagnosticCategory, INTRINSICS_VIRTUAL_PATH,
    Profile, VfsLimits,
};
use tsrs_vfs::FS;

#[test]
fn profile_loads_and_validates() {
    let profile = Profile::load().expect("embedded profile should load and validate");
    assert_eq!(profile.intrinsics_module, "@norn/authoring");
    assert!(
        profile.admitted_entry("@norn/authoring#Norn.fn").is_some(),
        "Norn.fn must be admitted in the profile"
    );
    assert!(
        profile.admitted_entry("@norn/authoring#Norn.type").is_some(),
        "Norn.type must be admitted in the profile"
    );
    assert_eq!(
        profile.code_for("effect/Effect#runPromise"),
        "NORN-FLOW-UNSUPPORTED-CALL"
    );
}

#[test]
fn bounded_vfs_enforces_quotas_fail_closed() {
    let limits = VfsLimits {
        max_files: 2,
        max_total_bytes: 32,
        max_file_bytes: 20,
    };
    let mut vfs = BoundedVfs::new(limits);
    assert!(vfs.file_exists(INTRINSICS_VIRTUAL_PATH));
    vfs.add_file("/a.ts", "export const a = 1")
        .expect("first file fits");
    assert!(!vfs.quota_breached());

    let err = vfs
        .add_file("/b.ts", "export const too_long_for_single_file = 12345")
        .expect_err("exceeding max_file_bytes must fail closed");
    assert!(matches!(err, AuthoringError::VfsQuotaExceeded(_)));
    assert!(vfs.quota_breached());

    // Once breached, reads and subsequent inserts fail closed.
    assert!(!vfs.file_exists("/a.ts"));
    assert!(vfs.read_file("/a.ts").is_none());
    let second = vfs.add_file("/c.ts", "1");
    assert!(matches!(second, Err(AuthoringError::VfsQuotaExceeded(_))));

    // Path traversal is rejected.
    let mut clean_vfs = BoundedVfs::default();
    let traversal = clean_vfs.add_file("/a/../escape.ts", "export const x = 1");
    assert!(matches!(traversal, Err(AuthoringError::InvalidPath { .. })));
}

#[test]
fn compiles_norn_authoring_import_and_resolves_symbol_identity() {
    let profile = Profile::load().expect("profile loads");
    let mut vfs = BoundedVfs::default();
    let source = r#"import { Norn, Norn as N } from "@norn/authoring"

export const directRef = Norn.fn
export const aliasedCall = N.fn<{ id: string }, { ok: boolean }>({
  id: "demo.ping",
  version: "1.0.0",
})
"#;
    let file = vfs
        .add_file("/app/definitions.ts", source)
        .expect("add definitions.ts");

    let program = AuthoringProgram::new(vfs, &[file.as_str()], profile)
        .expect("AuthoringProgram should construct");
    let diagnostics = program
        .all_diagnostics(Some(file.as_str()))
        .expect("diagnostics should collect");
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|diag| diag.category == DiagnosticCategory::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "expected zero compiler errors, got: {errors:#?}"
    );

    let resolved = program
        .resolve_initializer_identities(file.as_str())
        .expect("initializer identities should resolve");
    let direct = resolved
        .iter()
        .find(|(name, _)| name == "directRef")
        .and_then(|(_, id)| id.as_ref())
        .expect("directRef should resolve");
    assert_eq!(direct.id, "@norn/authoring#Norn.fn");
    assert_eq!(direct.declaring_file, INTRINSICS_VIRTUAL_PATH);
    assert_eq!(direct.qualified_name, "Norn.fn");

    let aliased = resolved
        .iter()
        .find(|(name, _)| name == "aliasedCall")
        .and_then(|(_, id)| id.as_ref())
        .expect("aliasedCall should resolve through alias N");
    assert_eq!(aliased.id, "@norn/authoring#Norn.fn");
    assert_eq!(aliased.declaring_file, INTRINSICS_VIRTUAL_PATH);
}
