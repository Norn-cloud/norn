//! Smoke and boundary tests for `Profile`, `BoundedVfs`, and `AuthoringProgram`.

use norn_authoring::{
    AuthoringError, AuthoringProgram, BoundedVfs, DiagnosticCategory, INTRINSICS_VIRTUAL_PATH,
    PINNED_EFFECT_VIRTUAL_ROOT, Profile, ProfileChecker, VfsLimits,
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
    vfs.add_pinned_file(
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

#[test]
fn caller_cannot_spoof_reserved_pinned_namespaces_or_effect_prefix()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let mut vfs = BoundedVfs::default();

    // Reserved pinned namespaces reject caller files even before any disk mount.
    let reserved_effect_dts = format!("{PINNED_EFFECT_VIRTUAL_ROOT}/dist/Effect.d.ts");
    let spoof_effect = vfs.add_file(
        &reserved_effect_dts,
        "export declare function gen(): void\n",
    );
    assert!(matches!(
        spoof_effect,
        Err(AuthoringError::InvalidPath { .. })
    ));
    assert!(!vfs.file_exists(&reserved_effect_dts));

    let spoof_intrinsics = vfs.add_file(
        "/@norn/authoring/extra.d.ts",
        "export declare const spoof: number\n",
    );
    assert!(matches!(
        spoof_intrinsics,
        Err(AuthoringError::InvalidPath { .. })
    ));
    assert!(!vfs.file_exists("/@norn/authoring/extra.d.ts"));

    // Even if a caller file sits under a custom `with_effect_dist_prefix`,
    // both `resolve_profile_module` and `ProfileChecker` import admission require
    // verified pinned membership (`is_pinned_file`).
    vfs.add_file(
        "/app/node_modules/effect/package.json",
        "{\"name\":\"effect\",\"exports\":{\"./Effect\":{\"types\":\"./dist/Effect.d.ts\"}}}\n",
    )?;
    vfs.add_file(
        "/app/node_modules/effect/dist/Effect.d.ts",
        "export declare function gen(): void\n",
    )?;
    let entry = vfs.add_file(
        "/app/use_fake.ts",
        "import * as Effect from \"effect/Effect\"\nexport const spoofed = Effect.gen\n",
    )?;
    let program = AuthoringProgram::new(vfs, &[entry.as_str()], profile.clone())?
        .with_effect_dist_prefix("/app/node_modules/effect/dist/");
    let resolved = program.resolve_initializer_identities(entry.as_str())?;
    let spoofed = resolved
        .iter()
        .find(|(name, _)| name == "spoofed")
        .ok_or("spoofed initializer present")?;
    assert!(
        spoofed.1.is_none(),
        "caller-supplied file under custom effect_dist_prefix must never receive a profile ID: {:?}",
        spoofed.1
    );

    let checker = ProfileChecker::new(profile);
    let reports = checker.check_program(&program, &[entry.as_str()])?;
    let report = reports.first().ok_or("expected one FileReport")?;
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diag| diag.code == "NORN-FLOW-MODULE"),
        "caller-spoofed effect package must fail pinned module admission: {:?}",
        report.diagnostics
    );
    Ok(())
}

#[test]
fn failed_pinned_effect_mount_leaves_vfs_unmodified() -> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let tmp = std::env::temp_dir().join(format!(
        "norn-authoring-atomic-mount-{}",
        std::process::id()
    ));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    let dist_dir = tmp.join("dist");
    std::fs::create_dir_all(&dist_dir)?;
    std::fs::write(
        tmp.join("package.json"),
        "{\"name\":\"effect\",\"version\":\"0.0.0-wrong\"}\n",
    )?;
    std::fs::write(
        dist_dir.join("Effect.d.ts"),
        "export declare function gen(): void\n",
    )?;

    let mut vfs = BoundedVfs::default();
    let err = vfs.mount_pinned_effect_dir(&tmp, &profile);
    std::fs::remove_dir_all(&tmp)?;

    let pkg_json = format!("{PINNED_EFFECT_VIRTUAL_ROOT}/package.json");
    let effect_dts = format!("{PINNED_EFFECT_VIRTUAL_ROOT}/dist/Effect.d.ts");
    assert!(matches!(err, Err(AuthoringError::PinMismatch { .. })));
    assert!(!vfs.file_exists(&pkg_json));
    assert!(!vfs.file_exists(&effect_dts));
    assert!(!vfs.is_pinned_file(&effect_dts));
    Ok(())
}

#[cfg(unix)]
#[test]
fn pinned_mount_ignores_internal_symlinks() -> Result<(), Box<dyn std::error::Error>> {
    let pid = std::process::id();
    let base = std::env::temp_dir().join(format!("norn-authoring-symlink-mount-{pid}"));
    if base.exists() {
        std::fs::remove_dir_all(&base)?;
    }
    let pkg_dir = base.join("pkg");
    let dist_dir = pkg_dir.join("dist");
    std::fs::create_dir_all(&dist_dir)?;
    let secret = base.join("outside.d.ts");
    std::fs::write(&secret, "export declare const leaked: string\n")?;
    std::fs::write(
        pkg_dir.join("package.json"),
        "{\"name\":\"effect\",\"version\":\"1.0.0\"}\n",
    )?;
    std::fs::write(
        dist_dir.join("Effect.d.ts"),
        "export declare function gen(): void\n",
    )?;
    std::os::unix::fs::symlink(&secret, dist_dir.join("Leaked.d.ts"))?;

    let mut vfs = BoundedVfs::default();
    let mounted = vfs.mount_pinned_package_dir("/__norn_pinned__/symtest", &pkg_dir)?;
    std::fs::remove_dir_all(&base)?;

    assert_eq!(mounted, 2);
    assert!(vfs.is_pinned_file("/__norn_pinned__/symtest/dist/Effect.d.ts"));
    assert!(!vfs.file_exists("/__norn_pinned__/symtest/dist/Leaked.d.ts"));
    // Mounted prefix is reserved against caller writes.
    let caller_write = vfs.add_file(
        "/__norn_pinned__/symtest/dist/Caller.d.ts",
        "export declare const x: number\n",
    );
    assert!(matches!(
        caller_write,
        Err(AuthoringError::InvalidPath { .. })
    ));
    Ok(())
}

#[test]
fn generic_mount_rejects_effect_namespace_and_remount_replaces_stale_files()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let pid = std::process::id();
    let base = std::env::temp_dir().join(format!("norn-authoring-remount-{pid}"));
    if base.exists() {
        std::fs::remove_dir_all(&base)?;
    }

    let v1_dir = base.join("effect-v1");
    let v1_dist = v1_dir.join("dist");
    let v1_stale_dir = v1_dist.join("stale");
    std::fs::create_dir_all(&v1_stale_dir)?;
    let valid_pkg = format!(
        "{{\"name\":\"effect\",\"version\":\"{}\"}}\n",
        profile.pins.effect.version
    );
    std::fs::write(v1_dir.join("package.json"), &valid_pkg)?;
    std::fs::write(
        v1_dist.join("Effect.d.ts"),
        "export declare function gen(): void\n",
    )?;
    std::fs::write(
        v1_stale_dir.join("Legacy.d.ts"),
        "export declare function legacy(): void\n",
    )?;

    let v2_dir = base.join("effect-v2");
    let v2_dist = v2_dir.join("dist");
    std::fs::create_dir_all(&v2_dist)?;
    std::fs::write(v2_dir.join("package.json"), &valid_pkg)?;
    std::fs::write(
        v2_dist.join("Effect.d.ts"),
        "export declare function succeed(): void\n",
    )?;

    let mut vfs = BoundedVfs::default();

    // Generic mount API must reject `/node_modules/effect`, any descendant, and any ancestor.
    for forbidden in [
        PINNED_EFFECT_VIRTUAL_ROOT,
        "/node_modules/effect/sub",
        "/node_modules",
    ] {
        let res = vfs.mount_pinned_package_dir(forbidden, &v1_dir);
        assert!(
            matches!(res, Err(AuthoringError::InvalidPath { .. })),
            "expected InvalidPath for mount_pinned_package_dir({forbidden:?}), got {res:?}"
        );
    }

    // Also reject direct add_pinned_file under `/node_modules/effect/`.
    let direct_effect = vfs.add_pinned_file(
        "/node_modules/effect/dist/Effect.d.ts",
        "export declare function gen(): void\n",
    );
    assert!(matches!(
        direct_effect,
        Err(AuthoringError::InvalidPath { .. })
    ));

    // Mount v1 through the validated effect mount API.
    let first_count = vfs.mount_pinned_effect_dir(&v1_dir, &profile)?;
    assert_eq!(first_count, 3);
    let stale_file = format!("{PINNED_EFFECT_VIRTUAL_ROOT}/dist/stale/Legacy.d.ts");
    let stale_dir = format!("{PINNED_EFFECT_VIRTUAL_ROOT}/dist/stale");
    assert!(vfs.is_pinned_file(&stale_file));
    assert!(vfs.file_exists(&stale_file));
    assert!(vfs.directory_exists(&stale_dir));

    // Re-mount v2 over the same effect root: stale files and stale directories must vanish.
    let second_count = vfs.mount_pinned_effect_dir(&v2_dir, &profile)?;
    std::fs::remove_dir_all(&base)?;

    assert_eq!(second_count, 2);
    assert!(!vfs.is_pinned_file(&stale_file));
    assert!(!vfs.file_exists(&stale_file));
    assert!(vfs.read_file(&stale_file).is_none());
    assert!(!vfs.directory_exists(&stale_dir));
    let dist_entries = vfs.get_accessible_entries(&format!("{PINNED_EFFECT_VIRTUAL_ROOT}/dist"));
    assert_eq!(dist_entries.files, vec!["Effect.d.ts".to_owned()]);
    assert!(dist_entries.directories.is_empty());
    assert_eq!(
        vfs.read_file(&format!("{PINNED_EFFECT_VIRTUAL_ROOT}/dist/Effect.d.ts"))
            .as_deref(),
        Some("export declare function succeed(): void\n")
    );
    Ok(())
}
