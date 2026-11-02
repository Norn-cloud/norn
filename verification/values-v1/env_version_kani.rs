//! Checked proof of production environment-version dispatch, not JSON or SHA-256.
#[path = "../../crates/norn-expr/src/env/version.rs"]
mod version;

#[kani::proof]
fn val_env_versions_never_alias_or_accept_unknown_formats() {
    let number: u32 = kani::any();
    match version::TypeEnvVersion::from_number(number) {
        Some(version) => { assert!(number == 1 || number == 2); assert_eq!(version.number(), number); }
        None => assert!(number != 1 && number != 2),
    }
}
