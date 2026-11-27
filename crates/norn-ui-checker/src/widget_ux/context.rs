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

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    WidgetActionConfirmation, WidgetComponent, WidgetEffectHandler, WidgetMachine,
    WidgetMachineCall, WidgetMachineEffect, WidgetMessage, WidgetNode, WidgetOverrideValue,
    WidgetText, WidgetUx, WidgetValueType, widget_message_id,
};

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
        // Only the page whose context names the object reads it: no other page's machine, and no
        // catalog entry's, which any page may place.
        let mut machines: Vec<(String, Option<&str>, &WidgetMachine)> = Vec::new();
        for (index, page) in self.ux.pages.iter().enumerate() {
            if let Some(machine) = &page.machine {
                machines.push((
                    format!("/payload/ux/pages/{index}/machine"),
                    Some(page.page_id.as_str()),
                    machine,
                ));
            }
        }
        for (index, entry) in self.ux.catalog.iter().enumerate() {
            if let Some(machine) = &entry.machine {
                machines.push((
                    format!("/payload/ux/catalog/{index}/machine"),
                    None,
                    machine,
                ));
            }
        }
        for (base, page_id, machine) in machines {
            for (pointer, call) in machine_calls(machine, &base) {
                let WidgetMachineCall::Read { binding_id } = call else {
                    continue;
                };
                if self.ux.context_reads.iter().any(|read| {
                    &read.binding_id == binding_id && Some(read.page_id.as_str()) != page_id
                }) {
                    self.error(
                        &pointer,
                        "a context read is read only by the machine of the page whose context names its object",
                    );
                }
            }
        }
    }

    pub(super) fn check_messages(&self) {
        let mut found = Texts::default();
        found.ux(self.ux);
        // One id names one text in one context, so a translation keyed by it is never another's.
        let mut named: BTreeMap<&str, (&str, Option<&str>)> = BTreeMap::new();
        for (pointer, message) in found.0 {
            let (id, source, context) = (
                message.id.as_str(),
                message.source.as_str(),
                message.context.as_deref(),
            );
            if let Some(&(other, other_context)) = named.get(id)
                && (other, other_context) != (source, context)
            {
                self.error(
                    &format!("{pointer}/msg/id"),
                    format!(
                        "message `{id}` names two texts: `{source}` here and `{other}` elsewhere; give one of them a context"
                    ),
                );
            }
            named.entry(id).or_insert((source, context));
            if source.trim().is_empty() {
                self.error(
                    &format!("{pointer}/msg/source"),
                    "a message's source text must not be blank",
                );
            }
            if context.is_some_and(|context| context.trim().is_empty()) {
                self.error(
                    &format!("{pointer}/msg/context"),
                    "a message's context, when present, must not be blank",
                );
            }
            let Some(expected) = widget_message_id(source, context) else {
                let member = if source.contains('\0') {
                    "source"
                } else {
                    "context"
                };
                self.error(
                    &format!("{pointer}/msg/{member}"),
                    "a message's source text and context must not hold U+0000, which separates them in its id",
                );
                continue;
            };
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
                        format!("a catalog node is held in `{state_id}`, which its page's machine does not declare"),
                    );
                }
            }
        }
    }
}

/// Every call `machine` starts or awaits, with its pointer.
fn machine_calls<'m>(
    machine: &'m WidgetMachine,
    base: &str,
) -> Vec<(String, &'m WidgetMachineCall)> {
    let mut calls = Vec::new();
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
    calls
}

/// Every source message of a contract, with its pointer: a walk of the typed [`WidgetText`]
/// members, so a JSON value (a machine field's `initial`, a filter literal) is never read as one.
#[derive(Default)]
struct Texts<'u>(Vec<(String, &'u WidgetMessage)>);

impl<'u> Texts<'u> {
    fn text(&mut self, pointer: String, text: &'u WidgetText) {
        if let WidgetText::Message(message) = text {
            self.0.push((pointer, &message.msg));
        }
    }

    fn confirmation(&mut self, pointer: &str, confirmation: &'u WidgetActionConfirmation) {
        if let Some(message) = &confirmation.message {
            self.text(format!("{pointer}/message"), message);
        }
        if let Some(phrase) = &confirmation.phrase {
            self.text(format!("{pointer}/phrase"), phrase);
        }
    }

    fn handlers(&mut self, pointer: &str, handlers: &'u [WidgetEffectHandler]) {
        for (index, handler) in handlers.iter().enumerate() {
            if let WidgetEffectHandler::ConfirmGuard { confirmation } = handler {
                self.confirmation(&format!("{pointer}/{index}/confirmation"), confirmation);
            }
        }
    }

    fn node(&mut self, pointer: &str, node: &'u WidgetNode) {
        match &node.component {
            WidgetComponent::Section { section } => {
                self.text(format!("{pointer}/component/section/label"), &section.label);
            }
            WidgetComponent::ChildGrid { label, .. } | WidgetComponent::Control { label, .. } => {
                self.text(format!("{pointer}/component/label"), label);
            }
            WidgetComponent::List { .. }
            | WidgetComponent::Detail { .. }
            | WidgetComponent::Dialog { .. }
            | WidgetComponent::Form { .. }
            | WidgetComponent::Field { .. }
            | WidgetComponent::Action { .. }
            | WidgetComponent::Navigation { .. }
            | WidgetComponent::Catalog { .. }
            | WidgetComponent::Composition { .. } => {}
        }
        self.handlers(&format!("{pointer}/handlers"), &node.handlers);
        for (index, child) in node.children.iter().enumerate() {
            self.node(&format!("{pointer}/children/{index}"), child);
        }
    }

    fn ux(&mut self, ux: &'u WidgetUx) {
        let base = "/payload/ux";
        for (index, page) in ux.pages.iter().enumerate() {
            self.node(&format!("{base}/pages/{index}/root"), &page.root);
        }
        for (index, field) in ux.fields.iter().enumerate() {
            self.text(format!("{base}/fields/{index}/label"), &field.label);
            if let Some(help) = &field.help {
                self.text(format!("{base}/fields/{index}/help"), help);
            }
        }
        for (index, form) in ux.forms.iter().enumerate() {
            let at = format!("{base}/forms/{index}/submit");
            self.text(format!("{at}/label"), &form.submit.label);
            self.confirmation(&format!("{at}/confirmation"), &form.submit.confirmation);
            self.handlers(&format!("{at}/handlers"), &form.submit.handlers);
        }
        for (index, placement) in ux.action_placements.iter().enumerate() {
            let at = format!("{base}/actionPlacements/{index}");
            self.text(format!("{at}/label"), &placement.label);
            self.confirmation(&format!("{at}/confirmation"), &placement.confirmation);
            self.handlers(&format!("{at}/handlers"), &placement.handlers);
        }
        if let Some(shell) = &ux.shell_handlers {
            self.handlers(&format!("{base}/shellHandlers/handlers"), &shell.handlers);
        }
        for (index, item) in ux.navigation_items.iter().enumerate() {
            self.text(format!("{base}/navigationItems/{index}/label"), &item.label);
        }
        for (index, change) in ux.overrides.iter().enumerate() {
            if let WidgetOverrideValue::Label { value } = &change.value {
                self.text(format!("{base}/overrides/{index}/value/value"), value);
            }
        }
        for (index, entry) in ux.catalog.iter().enumerate() {
            self.text(format!("{base}/catalog/{index}/label"), &entry.label);
        }
    }
}

/// Each state a catalog node that is no UI SDK layer (an island, or a primitive no person dismisses)
/// is held in, with its pointer. A layer's states answer to its own law (`ui_layer`).
fn islands_held<'n>(node: &'n WidgetNode, pointer: &str, out: &mut Vec<(String, &'n str)>) {
    let layer = crate::ui_sdk::primitive_of(node)
        .is_some_and(|(_, primitive)| !primitive.dismiss_attempts.is_empty());
    if let WidgetComponent::Catalog { held_in, .. } = &node.component
        && !layer
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
