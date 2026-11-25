//! Context reads (UI-CONTEXT-READ-1), source messages (UI-TEXT-1) and the states a catalog island
//! is drawn in.
//!
//! - A context read names the object its page's context supplies: two required text parameters of
//!   that page, the object type and its key, with no entity bound to either. Only that page's
//!   machine reads it, and no property, field, form, filter or list names it: the bindings those
//!   resolve against are the data bindings, which never hold a context read's id.
//! - A source message's id is the hash of its source text and context (`norn_ir::message_id`), so
//!   a translation keyed by the id is the translation of exactly that text in that context.
//! - A catalog island held in machine states (`heldIn`) is drawn only in them, so each one is a
//!   state its page's machine declares.

use std::collections::BTreeSet;

use norn_ir::{
    WidgetComponent, WidgetMachineCall, WidgetMachineEffect, WidgetNode, WidgetValueType,
    widget_message_id,
};
use serde_json::Value;

use super::UxChecker;

impl UxChecker<'_, '_, '_> {
    pub(super) fn check_context_reads(&self) {
        let mut ids: BTreeSet<&str> = self
            .ux
            .data_bindings
            .iter()
            .map(|binding| binding.binding_id.as_str())
            .collect();
        for (index, read) in self.ux.context_reads.iter().enumerate() {
            let path = format!("/payload/ux/contextReads/{index}");
            if read.binding_id.trim().is_empty() || !ids.insert(read.binding_id.as_str()) {
                self.error(
                    &format!("{path}/bindingId"),
                    "a context read's id must be nonblank and unique among data bindings and context reads",
                );
            }
            let Some(page) = self.pages.get(read.page_id.as_str()).copied() else {
                self.error(
                    &format!("{path}/pageId"),
                    "a context read names a page this contract does not declare",
                );
                continue;
            };
            if read.type_parameter_id == read.key_parameter_id {
                self.error(
                    &format!("{path}/keyParameterId"),
                    "a context read's type and key are two parameters",
                );
            }
            for (member, parameter_id) in [
                ("typeParameterId", &read.type_parameter_id),
                ("keyParameterId", &read.key_parameter_id),
            ] {
                let parameter = page
                    .context_params
                    .iter()
                    .find(|param| &param.param_id == parameter_id);
                let fits = parameter.is_some_and(|param| {
                    param.required
                        && param.value_type == WidgetValueType::Text
                        && param.runtime_key_of.is_none()
                });
                if !fits {
                    self.error(
                        &format!("{path}/{member}"),
                        "a context read names a required text context parameter of its page that is no entity's runtime key",
                    );
                }
            }
        }
        // Only the page whose context names the object reads it.
        for (page_index, page) in self.ux.pages.iter().enumerate() {
            let Some(machine) = &page.machine else {
                continue;
            };
            let base = format!("/payload/ux/pages/{page_index}/machine");
            let mut calls: Vec<(String, &WidgetMachineCall)> = Vec::new();
            for (index, state) in machine.states.iter().enumerate() {
                if let Some(call) = &state.awaits {
                    calls.push((format!("{base}/states/{index}/awaits"), call));
                }
            }
            for (index, transition) in machine.transitions.iter().enumerate() {
                for (at, effect) in transition.effects.iter().enumerate() {
                    if let WidgetMachineEffect::Start { call, .. } = effect {
                        calls.push((
                            format!("{base}/transitions/{index}/effects/{at}/call"),
                            call,
                        ));
                    }
                }
            }
            for (pointer, call) in calls {
                let WidgetMachineCall::Read { binding_id } = call else {
                    continue;
                };
                if self
                    .ux
                    .context_reads
                    .iter()
                    .any(|read| &read.binding_id == binding_id && read.page_id != page.page_id)
                {
                    self.error(
                        &pointer,
                        "a context read is read only by the page whose context names its object",
                    );
                }
            }
        }
    }

    pub(super) fn check_messages(&self) {
        let Ok(value) = serde_json::to_value(self.ux) else {
            return;
        };
        let mut found = Vec::new();
        messages(&value, "/payload/ux", &mut found);
        for (pointer, id, source, context) in found {
            if source.trim().is_empty() {
                self.error(
                    &format!("{pointer}/msg/source"),
                    "a message's source text must not be blank",
                );
            }
            if context
                .as_ref()
                .is_some_and(|context| context.trim().is_empty())
            {
                self.error(
                    &format!("{pointer}/msg/context"),
                    "a message's context, when present, must not be blank",
                );
            }
            let expected = widget_message_id(&source, context.as_deref());
            if id != expected {
                self.error(
                    &format!("{pointer}/msg/id"),
                    format!(
                        "a message's id is the hash of its source text and context: `{expected}`, not `{id}`"
                    ),
                );
            }
        }
    }

    pub(super) fn check_island_states(&self) {
        for (page_index, page) in self.ux.pages.iter().enumerate() {
            let states: BTreeSet<&str> =
                page.machine.as_ref().map_or_else(BTreeSet::new, |machine| {
                    machine
                        .states
                        .iter()
                        .map(|state| state.state_id.as_str())
                        .collect()
                });
            let mut held = Vec::new();
            islands_held(
                &page.root,
                &format!("/payload/ux/pages/{page_index}/root"),
                &mut held,
            );
            for (pointer, state_id) in held {
                if !states.contains(state_id) {
                    self.error(
                        &pointer,
                        format!("an island is held in `{state_id}`, which its page's machine does not declare"),
                    );
                }
            }
        }
    }
}

/// Every `{ "msg": { "id", "source", "context"? } }` in `value`, with its pointer.
fn messages(value: &Value, pointer: &str, out: &mut Vec<(String, String, String, Option<String>)>) {
    match value {
        Value::Object(members) => {
            if members.len() == 1
                && let Some(Value::Object(msg)) = members.get("msg")
                && let (Some(Value::String(id)), Some(Value::String(source))) =
                    (msg.get("id"), msg.get("source"))
            {
                let context = msg
                    .get("context")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                out.push((pointer.to_owned(), id.clone(), source.clone(), context));
                return;
            }
            for (key, member) in members {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                messages(member, &format!("{pointer}/{escaped}"), out);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                messages(item, &format!("{pointer}/{index}"), out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Each state a catalog island (a catalog node that is no UI SDK layer) is held in, with its pointer.
fn islands_held<'n>(node: &'n WidgetNode, pointer: &str, out: &mut Vec<(String, &'n str)>) {
    if let WidgetComponent::Catalog { held_in, .. } = &node.component
        && crate::ui_sdk::primitive_of(node).is_none()
    {
        for (index, state_id) in held_in.iter().enumerate() {
            out.push((
                format!("{pointer}/component/heldIn/{index}"),
                state_id.as_str(),
            ));
        }
    }
    for (index, child) in node.children.iter().enumerate() {
        islands_held(child, &format!("{pointer}/children/{index}"), out);
    }
}
