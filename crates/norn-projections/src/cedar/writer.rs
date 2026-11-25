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
