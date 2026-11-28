use norn_authoring::{
    AuthoringError, AuthoringProgram, BoundedVfs, DiagnosticCategory, INTRINSICS_VIRTUAL_PATH,
    Profile, VfsLimits,
};
use tsrs_vfs::FS;

#[test]
fn profile_loads_and_validates() -> Result<(), AuthoringError> {
    let profile = Profile::load()?;
    assert_eq!(profile.intrinsics_module, "@norn/authoring");
    assert!(
        profile.admitted_entry("@norn/authoring#Norn.fn").is_some(),
        "Norn.fn must be admitted in the profile"
    );
    assert!(
        profile
            .admitted_entry("@norn/authoring#Norn.type")
            .is_some(),
        "Norn.type must be admitted in the profile"
    );
    assert_eq!(
        profile.code_for("effect/Effect#runPromise"),
        "NORN-FLOW-UNSUPPORTED-CALL"
    );
    Ok(())
}

#[test]
fn bounded_vfs_enforces_quotas_fail_closed() -> Result<(), AuthoringError> {
    let limits = VfsLimits {
        max_files: 2,
        max_total_bytes: 32,
        max_file_bytes: 20,
        ..VfsLimits::default()
    };
    let mut vfs = BoundedVfs::new(limits);
    assert!(vfs.file_exists(INTRINSICS_VIRTUAL_PATH));
    vfs.add_file("/a.ts", "export const a = 1")?;
    assert!(!vfs.quota_breached());

    let err = vfs.add_file("/b.ts", "export const too_long_for_single_file = 12345");
    assert!(matches!(err, Err(AuthoringError::VfsQuotaExceeded(_))));
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
    Ok(())
}

#[test]
fn bounded_vfs_enforces_path_byte_and_depth_bounds_fail_closed() -> Result<(), AuthoringError> {
    let mut depth_vfs = BoundedVfs::new(VfsLimits {
        max_path_depth: 3,
        ..VfsLimits::default()
    });
    depth_vfs.add_file("/a/b/c.ts", "")?;
    let deep_err = depth_vfs.add_file("/a/b/c/d.ts", "");
    assert!(matches!(deep_err, Err(AuthoringError::VfsQuotaExceeded(_))));
    assert!(depth_vfs.quota_breached());
    assert!(!depth_vfs.file_exists("/a/b/c.ts"));

    let mut len_vfs = BoundedVfs::new(VfsLimits {
        max_path_bytes: 12,
        ..VfsLimits::default()
    });
    let long_err = len_vfs.add_file("/very/long/path.ts", "");
    assert!(matches!(long_err, Err(AuthoringError::VfsQuotaExceeded(_))));
    assert!(len_vfs.quota_breached());
    Ok(())
}

#[test]
fn compiles_norn_authoring_import_and_resolves_symbol_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let mut vfs = BoundedVfs::default();
    let source = r#"import { Norn, Norn as N } from "@norn/authoring"

export const directRef = Norn.fn
export const aliasedCall = N.fn<{ id: string }, { ok: boolean }>({
  id: "demo.ping",
  version: "1.0.0",
})
"#;
    let file = vfs.add_file("/app/definitions.ts", source)?;

    let program = AuthoringProgram::new(vfs, &[file.as_str()], profile)?;
    let diagnostics = program.all_diagnostics(Some(file.as_str()))?;
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|diag| diag.category == DiagnosticCategory::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "expected zero compiler errors, got: {errors:#?}"
    );

    let resolved = program.resolve_initializer_identities(file.as_str())?;
    let direct = resolved
        .iter()
        .find(|(name, _)| name == "directRef")
        .and_then(|(_, id)| id.as_ref())
        .ok_or("directRef should resolve")?;
    assert_eq!(direct.id, "@norn/authoring#Norn.fn");
    assert_eq!(direct.declaring_file, INTRINSICS_VIRTUAL_PATH);
    assert_eq!(direct.qualified_name, "Norn.fn");

    let aliased = resolved
        .iter()
        .find(|(name, _)| name == "aliasedCall")
        .and_then(|(_, id)| id.as_ref())
        .ok_or("aliasedCall should resolve through alias N")?;
    assert_eq!(aliased.id, "@norn/authoring#Norn.fn");
    assert_eq!(aliased.declaring_file, INTRINSICS_VIRTUAL_PATH);
    Ok(())
}

#[test]
fn lookalike_node_modules_effect_does_not_receive_profile_id()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let mut vfs = BoundedVfs::default();
    vfs.add_file(
        "/app/node_modules/effect/Effect.d.ts",
        "export declare function gen(): void\n",
    )?;
    vfs.add_file(
        "/__norn_pinned__/effect/dist/Effect.d.ts",
        "export declare function gen(): void\n",
    )?;
    let source = r#"import { gen as fakeGen } from "./node_modules/effect/Effect"
import { gen as realGen } from "../__norn_pinned__/effect/dist/Effect"

export const spoofed = fakeGen
export const pinned = realGen
"#;
    let file = vfs.add_file("/app/lookalike.ts", source)?;

    let program = AuthoringProgram::new(vfs, &[file.as_str()], profile)?
        .with_effect_dist_prefix("/__norn_pinned__/effect/dist/");

    let resolved = program.resolve_initializer_identities(file.as_str())?;

    let spoofed = resolved
        .iter()
        .find(|(name, _)| name == "spoofed")
        .ok_or("spoofed initializer present")?;
    assert!(
        spoofed.1.is_none(),
        "lookalike /app/node_modules/effect/Effect.d.ts must never receive a profile ID: {:?}",
        spoofed.1
    );

    let pinned = resolved
        .iter()
        .find(|(name, _)| name == "pinned")
        .and_then(|(_, id)| id.as_ref())
        .ok_or("pinned Effect.d.ts should resolve to profile ID")?;
    assert_eq!(pinned.id, "effect/Effect#gen");
    assert_eq!(
        pinned.declaring_file,
        "/__norn_pinned__/effect/dist/Effect.d.ts"
    );
    Ok(())
}
