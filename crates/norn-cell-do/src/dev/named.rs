//! DEV-PRINCIPALS-1: the manifest's named local principals, each holding
//! exactly its declared roles, and their session token files.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use super::{Failure, ManifestDev, policy};

/// One named local principal (DEV-PRINCIPALS-1).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManifestPrincipal {
    pub(super) actor: String,
    pub(super) roles: Vec<String>,
}

/// At most this many named principals: each is a token file and a session.
pub(super) const MAX_PRINCIPALS: usize = 16;

/// The manifest's named principals and their roles, or why they are invalid:
/// each actor is non-empty, distinct and neither the developer nor the
/// reviewer, and holds at least one non-empty role, none twice.
pub(super) fn named_principals(dev: &ManifestDev) -> Result<policy::NamedPrincipals, Failure> {
    let invalid = |message: String| Failure::setup("norn.project.invalid_manifest", message);
    if dev.principals.len() > MAX_PRINCIPALS {
        return Err(invalid(format!(
            "norn.json names {} dev.principals; at most {MAX_PRINCIPALS}",
            dev.principals.len()
        )));
    }
    // `developer` and `reviewer` also name the built-in principals to the
    // local Studio (`--as developer`, `--as reviewer`), so no named principal
    // takes either name.
    let reserved = [
        dev.actor.clone(),
        policy::reviewer(&dev.actor),
        "developer".to_owned(),
        "reviewer".to_owned(),
    ];
    let mut named = std::collections::BTreeMap::new();
    for principal in &dev.principals {
        let actor = principal.actor.trim();
        if actor.is_empty() || actor != principal.actor || reserved.contains(&principal.actor) {
            return Err(invalid(format!(
                "dev.principals actor {:?} must be a non-empty name other than the developer and the reviewer",
                principal.actor
            )));
        }
        let mut roles = principal.roles.clone();
        roles.sort();
        roles.dedup();
        if roles.is_empty()
            || roles.len() != principal.roles.len()
            || roles
                .iter()
                .any(|role| role.trim().is_empty() || role.trim() != role)
        {
            return Err(invalid(format!(
                "dev.principals {:?} must hold at least one role, each a non-empty name, none twice",
                principal.actor
            )));
        }
        if named
            .insert(principal.actor.clone(), principal.roles.clone())
            .is_some()
        {
            return Err(invalid(format!(
                "dev.principals names {:?} twice",
                principal.actor
            )));
        }
    }
    Ok(Arc::new(named))
}

/// The session token file of the `index`th named principal (from 1).
pub(super) fn principal_token_file(index: usize) -> String {
    format!(".norn/dev.principal.{index}.token")
}

/// Remove every named principal's token file in `state` past the first
/// `keep`: a principal the manifest no longer names keeps no token on
/// disk. A symlink is removed, never followed.
pub(super) fn remove_stale_principal_tokens(state: &Path, keep: usize) -> Result<(), Failure> {
    let failed = |error: std::io::Error| {
        Failure::setup("norn.dev.io", format!("{}: {error}", state.display()))
    };
    for entry in std::fs::read_dir(state).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let name = entry.file_name();
        let dropped = name
            .to_str()
            .and_then(|name| name.strip_prefix("dev.principal."))
            .and_then(|rest| rest.strip_suffix(".token"))
            .filter(|index| !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|index| index.parse::<usize>().ok())
            .is_some_and(|index| index == 0 || index > keep);
        if dropped {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(failed(error)),
            }
        }
    }
    Ok(())
}
