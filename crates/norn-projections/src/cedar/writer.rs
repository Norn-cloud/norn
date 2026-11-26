//! The Cedar resource footprint of a synchronous writer (AUTH-SYNC-WRITER-1).

use norn_ir::{ActionEffectProgram, ActionRoot};

/// AUTH-SYNC-WRITER-1: commit authorizes every declared writer target of a
/// transition or (CREATE-WRITER-1) create-only writer action. Catalog
/// invocation targets stay tied to the primary effect.
pub(super) fn writer_resource_kinds<'a>(
    action: &'a ActionRoot,
    program: &ActionEffectProgram,
) -> impl Iterator<Item = &'a str> {
    let composed = action.synchronous_rule.is_some()
        && matches!(
            program,
            ActionEffectProgram::FireTransition { .. }
                | ActionEffectProgram::UpsertInputObject {
                    write_mode: norn_ir::UpsertWriteMode::CreateOnly,
                    ..
                }
        );
    action
        .writes
        .iter()
        .filter(move |_| composed)
        .map(|write| write.entity_kind.as_str())
}

/// Whether `namespace` / `cedar_name` collides with a hardcoded Norn host
/// namespace or entity emitted by `render_actor` / `project_host_access_schema`.
pub(super) fn is_reserved_host_type(namespace: &str, cedar_name: &str) -> bool {
    namespace == "Norn"
        || namespace.starts_with("Norn::")
        || matches!(
            cedar_name,
            "norn::Ledger"
                | "norn::Definitions"
                | "norn::Marking"
                | "norn::Source"
                | "norn::RefusalDetail"
        )
}

fn is_cedar_type_keyword(value: &str) -> bool {
    matches!(value, "entity" | "namespace" | "action" | "type")
}

pub(super) fn cedar_entity_name(name: &str) -> Option<String> {
    let (domain, local) = name.rsplit_once('.')?;
    if !is_cedar_identifier(local)
        || is_cedar_type_keyword(local)
        || !domain
            .split('.')
            .all(|seg| is_cedar_identifier(seg) && !is_cedar_type_keyword(seg))
    {
        return None;
    }
    Some(format!("{}::{local}", domain.replace('.', "::")))
}

pub(super) fn is_cedar_identifier(value: &str) -> bool {
    if matches!(
        value,
        "true" | "false" | "if" | "then" | "else" | "in" | "is" | "like" | "has" | "__cedar"
    ) {
        return false;
    }
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}
