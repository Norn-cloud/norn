//! UI effect laws, lexical handler subtraction, and lifecycle transition conditions.

use std::collections::BTreeSet;

use norn_ir::{
    ActionEffectProgram, ActionRoot, ActionUiEffect, ArtifactEnvelope, ArtifactKind,
    ArtifactPayload, WidgetActionConfirmation, WidgetComponent, WidgetDataBinding,
    WidgetEffectHandler, WidgetExpressionInputs, WidgetNode, WidgetOutcomeState,
    WidgetOverrideValue, WidgetUx, ZenCondition, ZenExpression,
};

use norn_checker::entity_instance::EffectiveObjectType;

use super::{UxChecker, dependency_matches};

/// Why a transition's canonical UI enabled condition could not be generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedTransitionEnabledWhenError {
    /// The supplied record binding id is blank.
    BlankRecordBinding,
    /// The action targets an unrelated object kind.
    ObjectTypeMismatch,
    /// The object's effective type does not expose the named lifecycle.
    MissingLifecycle,
    /// The lifecycle does not declare the action's transition id.
    UnknownTransition,
    /// The effective transition has no source states after narrowing.
    NoSourceStates,
    /// A state could not be encoded as a ZEN string literal.
    StateEncoding,
}

impl std::fmt::Display for GeneratedTransitionEnabledWhenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::BlankRecordBinding => "record binding id is blank",
            Self::ObjectTypeMismatch => "transition action targets an unrelated object type",
            Self::MissingLifecycle => "transition action names no effective lifecycle",
            Self::UnknownTransition => "transition action names an unknown lifecycle transition",
            Self::NoSourceStates => "effective lifecycle transition has no source states",
            Self::StateEncoding => {
                "effective lifecycle state could not be encoded as a ZEN string literal"
            }
        })
    }
}

impl std::error::Error for GeneratedTransitionEnabledWhenError {}

/// Generate the canonical `enabledWhen` condition for a lifecycle transition.
///
/// The condition reads only `input.record.<lifecycle property>` from the named
/// record binding and compares it with each effective source state. A
/// non-transition action has no generated condition. The effective object
/// type must be the transition's type or a subtype, so narrowed lifecycle
/// states are honored.
///
/// # Errors
///
/// Returns an error when the transition cannot be resolved on the effective
/// object type or has no effective source states.
pub fn generated_transition_enabled_when(
    action: &ActionRoot,
    object_type: &EffectiveObjectType,
    record_binding_id: &str,
) -> Result<Option<ZenCondition>, GeneratedTransitionEnabledWhenError> {
    let Some(ActionEffectProgram::FireTransition {
        object_kind,
        lifecycle,
        transition,
        ..
    }) = action.effect_program.as_ref()
    else {
        return Ok(None);
    };
    if record_binding_id.trim().is_empty() {
        return Err(GeneratedTransitionEnabledWhenError::BlankRecordBinding);
    }
    if !object_type
        .lineage
        .iter()
        .any(|ancestor| ancestor.as_str() == object_kind)
    {
        return Err(GeneratedTransitionEnabledWhenError::ObjectTypeMismatch);
    }
    let effective = object_type
        .lifecycle(lifecycle.as_str())
        .ok_or(GeneratedTransitionEnabledWhenError::MissingLifecycle)?;
    let transition = effective
        .lifecycle
        .transitions
        .iter()
        .find(|candidate| candidate.id == *transition)
        .ok_or(GeneratedTransitionEnabledWhenError::UnknownTransition)?;
    if transition.from.is_empty() {
        return Err(GeneratedTransitionEnabledWhenError::NoSourceStates);
    }
    let property = lifecycle.as_str();
    let expression = transition
        .from
        .iter()
        .map(|state| {
            let state = serde_json::to_string(state)
                .map_err(|_error| GeneratedTransitionEnabledWhenError::StateEncoding)?;
            Ok(format!("input.record.{property} == {state}"))
        })
        .collect::<Result<Vec<_>, GeneratedTransitionEnabledWhenError>>()?
        .join(" or ");
    Ok(Some(ZenCondition {
        expression: ZenExpression(expression),
        inputs: WidgetExpressionInputs {
            record_binding_id: Some(record_binding_id.to_owned()),
            draft_form_id: None,
            selection_binding_id: None,
            context_page_id: None,
            row_grid_id: None,
        },
    }))
}

#[derive(Clone, Copy)]
struct EffectScope<'a> {
    owner: &'a ArtifactEnvelope,
    ux: &'a WidgetUx,
    site: &'a EffectRenderSite,
    overrides: &'a [(&'a ArtifactEnvelope, &'a WidgetUx)],
    record: Option<&'a WidgetDataBinding>,
}

impl UxChecker<'_, '_, '_> {
    pub(super) fn check_effect_laws(&self) {
        if !self.enforce_effect_root {
            return;
        }
        if let Some(shell) = &self.ux.shell_handlers {
            self.check_effect_handlers(&shell.handlers, "/payload/ux/shellHandlers/handlers", true);
        }
        for (index, page) in self.ux.pages.iter().enumerate() {
            let path = format!("/payload/ux/pages/{index}/root");
            let site = EffectRenderSite {
                page_id: page.page_id.clone(),
                pointer: path.clone(),
                path,
            };
            let mut stack = BTreeSet::new();
            stack.insert((
                self.owner.artifact_id.clone(),
                self.owner.version.clone(),
                page.page_id.clone(),
            ));
            let mut invocations = self.effect_tree(
                &EffectScope {
                    owner: self.owner,
                    ux: self.ux,
                    site: &site,
                    overrides: &[],
                    record: None,
                },
                &page.root,
                &mut stack,
            );
            if let Some(shell) = &self.ux.shell_handlers {
                for invocation in &mut invocations {
                    subtract_handlers(invocation, &shell.handlers, true);
                }
            }
            for invocation in invocations {
                for effect in &invocation.effects {
                    self.error(
                        &invocation.pointer,
                        format!(
                            "UI law: action {:?} has unhandled {} at render path {}",
                            invocation.action_id,
                            effect_label(effect),
                            invocation.path
                        ),
                    );
                }
            }
        }
    }

    // Expansion retains the real host path; only its shell is applied at the root.
    fn effect_tree(
        &self,
        scope: &EffectScope<'_>,
        node: &WidgetNode,
        stack: &mut BTreeSet<(String, String, String)>,
    ) -> Vec<EffectInvocation> {
        let &EffectScope {
            owner,
            ux,
            site,
            overrides,
            record,
        } = scope;
        let path = &site.path;
        let record = Self::effect_tree_record(ux, node, record);
        let scoped = EffectScope {
            owner,
            ux,
            site,
            overrides,
            record,
        };
        let mut invocations = Vec::new();
        match &node.component {
            WidgetComponent::Action { placement_id } => {
                if let Some(invocation) = self.effect_tree_action_node(&scoped, node, placement_id)
                {
                    invocations.push(invocation);
                }
            }
            WidgetComponent::Form { form_id } => {
                if let Some(invocation) =
                    self.effect_tree_form_node(owner, ux, form_id, site, record)
                {
                    invocations.push(invocation);
                }
            }
            WidgetComponent::Composition { widget, page_id } => {
                invocations.extend(self.effect_tree_composition(&scoped, widget, page_id, stack));
            }
            WidgetComponent::Section { .. }
            | WidgetComponent::List { .. }
            | WidgetComponent::Detail { .. }
            | WidgetComponent::Dialog { .. }
            | WidgetComponent::ChildGrid { .. }
            | WidgetComponent::Field { .. }
            | WidgetComponent::Navigation { .. }
            | WidgetComponent::Control { .. }
            | WidgetComponent::Catalog { .. } => {}
        }
        for (index, child) in node.children.iter().enumerate() {
            let child_site = site.child(index);
            invocations.extend(self.effect_tree(
                &EffectScope {
                    site: &child_site,
                    ..scoped
                },
                child,
                stack,
            ));
        }
        self.check_effect_handlers_at(
            &node.handlers,
            &format!("{path}/handlers"),
            false,
            (site.pointer != site.path).then_some(site.pointer.as_str()),
        );
        for invocation in &mut invocations {
            invocation.unknown_covered |=
                complete_unknown_surface(&node.handlers, &invocation.currencies);
            subtract_handlers(invocation, &node.handlers, false);
        }
        if let WidgetComponent::Dialog {
            binding_id,
            blocking,
            dismissal_methods,
        } = &node.component
        {
            self.check_dialog_effect_surface(
                owner,
                binding_id.as_deref(),
                *blocking,
                dismissal_methods,
                &invocations,
                site,
            );
        }
        invocations
    }

    fn effect_tree_record<'a>(
        ux: &'a WidgetUx,
        node: &WidgetNode,
        record: Option<&'a WidgetDataBinding>,
    ) -> Option<&'a WidgetDataBinding> {
        match &node.component {
            WidgetComponent::List {
                data_binding_id, ..
            }
            | WidgetComponent::Detail { data_binding_id } => ux
                .data_bindings
                .iter()
                .find(|binding| binding.binding_id == *data_binding_id),
            WidgetComponent::Form { form_id } => ux
                .forms
                .iter()
                .find(|form| form.form_id == *form_id)
                .and_then(|form| form.data_binding_id.as_ref())
                .and_then(|id| {
                    ux.data_bindings
                        .iter()
                        .find(|binding| binding.binding_id == *id)
                }),
            WidgetComponent::Section { .. }
            | WidgetComponent::Dialog { .. }
            | WidgetComponent::ChildGrid { .. }
            | WidgetComponent::Field { .. }
            | WidgetComponent::Action { .. }
            | WidgetComponent::Navigation { .. }
            | WidgetComponent::Control { .. }
            | WidgetComponent::Catalog { .. }
            | WidgetComponent::Composition { .. } => record,
        }
    }

    fn effect_tree_composition(
        &self,
        scope: &EffectScope<'_>,
        widget: &norn_ir::WidgetArtifactRef,
        page_id: &str,
        stack: &mut BTreeSet<(String, String, String)>,
    ) -> Vec<EffectInvocation> {
        let &EffectScope {
            owner,
            ux,
            site,
            overrides,
            record,
        } = scope;
        let Some(artifact) = self.find_exact(&widget.id, &widget.version, &ArtifactKind::Widget)
        else {
            return Vec::new();
        };
        let ArtifactPayload::Widget(payload) = &artifact.payload else {
            return Vec::new();
        };
        let Some(raw) = payload.ux.as_value() else {
            return Vec::new();
        };
        let Ok(composed) = serde_json::to_value(raw).and_then(serde_json::from_value::<WidgetUx>)
        else {
            return Vec::new();
        };
        let Some(page) = composed.pages.iter().find(|page| page.page_id == page_id) else {
            return Vec::new();
        };
        let identity = (
            artifact.artifact_id.clone(),
            artifact.version.clone(),
            page_id.to_owned(),
        );
        if !stack.insert(identity.clone()) {
            return Vec::new();
        }
        let mut scopes = overrides.to_vec();
        scopes.push((owner, ux));
        let composed_site = site.composed(page_id);
        let invocations = self.effect_tree(
            &EffectScope {
                owner: artifact,
                ux: &composed,
                site: &composed_site,
                overrides: &scopes,
                record,
            },
            &page.root,
            stack,
        );
        stack.remove(&identity);
        invocations
    }

    fn effect_tree_action_node(
        &self,
        scope: &EffectScope<'_>,
        node: &WidgetNode,
        placement_id: &str,
    ) -> Option<EffectInvocation> {
        let &EffectScope {
            owner,
            ux,
            site,
            overrides,
            record,
        } = scope;
        let replacement = node.semantic_id.as_ref().and_then(|id| {
            overrides.iter().find_map(|(scope_owner, scope_ux)| {
                scope_ux.overrides.iter().find_map(|override_| {
                    if override_.target_semantic_id != *id {
                        return None;
                    }
                    let WidgetOverrideValue::ActionPlacement { placement_id } = &override_.value
                    else {
                        return None;
                    };
                    Some((*scope_owner, *scope_ux, placement_id.as_str()))
                })
            })
        });
        let (placement_owner, placement_ux, id) = replacement.unwrap_or((owner, ux, placement_id));
        let placement = placement_ux
            .action_placements
            .iter()
            .find(|placement| placement.placement_id == id)?;
        let handler_path = placement_ux
            .action_placements
            .iter()
            .position(|candidate| candidate.placement_id == id)
            .map(|index| format!("/payload/ux/actionPlacements/{index}/handlers"));
        let anchor = (placement_owner.artifact_id != self.owner.artifact_id
            || placement_owner.version != self.owner.version)
            .then_some(site.pointer.as_str());
        self.check_effect_handlers_at(
            &placement.handlers,
            handler_path.as_deref().unwrap_or(&site.pointer),
            false,
            anchor,
        );
        let root = self.effect_action(
            placement_owner,
            placement_ux,
            &placement.binding_id,
            &site.pointer,
        );
        if let Some(root) = root.as_ref() {
            self.check_transition_condition(
                root,
                placement_ux,
                &placement.binding_id,
                placement.enabled_when.as_ref(),
                record,
                site,
            );
        }
        if placement.form_id.is_some() {
            if placement.retry {
                self.error(
                    &site.pointer,
                    "UI law: a form opener cannot expose retry because it invokes no action",
                );
            }
            return None;
        }
        let root = root.as_ref()?;
        let mut invocation = Self::effect_invocation(
            placement_owner,
            placement_ux,
            &placement.binding_id,
            root,
            site,
        )?;
        if placement.retry && !invocation.effects.contains(&ActionUiEffect::Idempotent) {
            self.error(
                &site.pointer,
                format!(
                    "UI law: retry requires idempotent capability for action {:?}",
                    invocation.action_id
                ),
            );
        }
        invocation.effects.remove(&ActionUiEffect::Idempotent);
        subtract_confirmation(&mut invocation.effects, &placement.confirmation);
        invocation.unknown_covered =
            complete_unknown_surface(&placement.handlers, &invocation.currencies);
        subtract_handlers(&mut invocation, &placement.handlers, false);
        Some(invocation)
    }

    fn effect_tree_form_node(
        &self,
        owner: &ArtifactEnvelope,
        ux: &WidgetUx,
        form_id: &str,
        site: &EffectRenderSite,
        record: Option<&WidgetDataBinding>,
    ) -> Option<EffectInvocation> {
        let form = ux.forms.iter().find(|form| form.form_id == form_id)?;
        let handler_path = ux
            .forms
            .iter()
            .position(|candidate| candidate.form_id == form_id)
            .map(|index| format!("/payload/ux/forms/{index}/submit/handlers"));
        let anchor = (owner.artifact_id != self.owner.artifact_id
            || owner.version != self.owner.version)
            .then_some(site.pointer.as_str());
        self.check_effect_handlers_at(
            &form.submit.handlers,
            handler_path.as_deref().unwrap_or(&site.pointer),
            false,
            anchor,
        );
        let root = self.effect_action(owner, ux, &form.submit_binding_id, &site.pointer)?;
        self.check_transition_condition(&root, ux, &form.submit_binding_id, None, record, site);
        let mut invocation =
            Self::effect_invocation(owner, ux, &form.submit_binding_id, &root, site)?;
        invocation.effects.remove(&ActionUiEffect::Idempotent);
        subtract_confirmation(&mut invocation.effects, &form.submit.confirmation);
        invocation.unknown_covered =
            complete_unknown_surface(&form.submit.handlers, &invocation.currencies);
        subtract_handlers(&mut invocation, &form.submit.handlers, false);
        Some(invocation)
    }

    fn check_dialog_effect_surface(
        &self,
        owner: &ArtifactEnvelope,
        binding_id: Option<&str>,
        blocking: bool,
        dismissal_methods: &[norn_ir::WidgetDismissalMethod],
        invocations: &[EffectInvocation],
        site: &EffectRenderSite,
    ) {
        let external: Vec<_> = invocations
            .iter()
            .filter(|invocation| invocation.external)
            .collect();
        if dismissal_methods.is_empty() {
            self.error(
                &site.member("component/dismissalMethods"),
                "UI law: every dialog requires at least one terminal dismissal method",
            );
        }
        if dismissal_methods.iter().collect::<BTreeSet<_>>().len() != dismissal_methods.len() {
            self.error(
                &site.member("component/dismissalMethods"),
                "UI law: dialog dismissal methods must be unique",
            );
        }
        if !external.is_empty() && !blocking {
            self.error(
                &site.member("component/blocking"),
                "UI law: a dialog containing an external invocation must be blocking",
            );
        }
        if blocking {
            if external.is_empty()
                || external.iter().any(|invocation| {
                    binding_id != Some(invocation.binding_id.as_str())
                        || invocation.owner_id != owner.artifact_id
                        || invocation.owner_version != owner.version
                })
                || external.first().is_some_and(|first| {
                    external.iter().any(|invocation| {
                        invocation.action_id != first.action_id
                            || invocation.action_version != first.action_version
                    })
                })
            {
                self.error(&site.member("component/bindingId"), "UI law: blocking dialog must bind its actual external descendant invocation; all external descendants must use that binding");
            }
            if external
                .iter()
                .any(|invocation| !invocation.unknown_covered)
            {
                self.error(&site.member("handlers"), "UI law: blocking dialog requires its own or descendant complete outcome surface including unknown outcome");
            }
        } else if binding_id.is_some() {
            self.error(
                &site.member("component/bindingId"),
                "UI law: only a blocking dialog may bind an external invocation",
            );
        }
    }

    pub(super) fn effect_action(
        &self,
        owner: &ArtifactEnvelope,
        ux: &WidgetUx,
        binding_id: &str,
        path: &str,
    ) -> Option<ActionRoot> {
        let binding = ux
            .action_bindings
            .iter()
            .find(|binding| binding.binding_id == binding_id)?;
        if !owner.dependencies.iter().any(|dependency| {
            dependency_matches(
                dependency,
                &binding.action.id,
                &binding.action.version,
                &ArtifactKind::Action,
            )
        }) {
            self.error(
                path,
                "UI law: invocation requires an exact required ActionIR dependency",
            );
            return None;
        }
        let artifact = self.find_exact(
            &binding.action.id,
            &binding.action.version,
            &ArtifactKind::Action,
        )?;
        let ArtifactPayload::Action(payload) = &artifact.payload else {
            return None;
        };
        let root = serde_json::to_value(payload.to_json_value())
            .and_then(serde_json::from_value::<ActionRoot>);
        match root {
            Ok(root) if crate::validate_artifact(artifact).is_ok() => Some(root),
            _ => {
                self.error(path, format!("UI law: referenced ActionIR {:?} is malformed; effect admission fails closed", binding.action.id));
                None
            }
        }
    }

    fn effect_invocation(
        owner: &ArtifactEnvelope,
        ux: &WidgetUx,
        binding_id: &str,
        root: &ActionRoot,
        site: &EffectRenderSite,
    ) -> Option<EffectInvocation> {
        let binding = ux
            .action_bindings
            .iter()
            .find(|binding| binding.binding_id == binding_id)?;
        let effects = root.effective_ui_effects();
        Some(EffectInvocation {
            action_id: binding.action.id.clone(),
            action_version: binding.action.version.clone(),
            binding_id: binding_id.to_owned(),
            path: site.path.clone(),
            pointer: site.pointer.clone(),
            owner_id: owner.artifact_id.clone(),
            owner_version: owner.version.clone(),
            external: effects.contains(&ActionUiEffect::External),
            currencies: effects
                .iter()
                .filter_map(|effect| {
                    if let ActionUiEffect::Money { currency_ref } = effect {
                        Some(currency_ref.clone())
                    } else {
                        None
                    }
                })
                .collect(),
            effects,
            unknown_covered: false,
        })
    }

    fn check_transition_condition(
        &self,
        root: &ActionRoot,
        ux: &WidgetUx,
        binding_id: &str,
        authored: Option<&ZenCondition>,
        record: Option<&WidgetDataBinding>,
        site: &EffectRenderSite,
    ) {
        if !matches!(
            root.effect_program,
            Some(ActionEffectProgram::FireTransition { .. })
        ) {
            return;
        }
        let record_binding = ux
            .action_bindings
            .iter()
            .find(|binding| binding.binding_id == binding_id)
            .and_then(|binding| binding.inputs.record_binding_id.as_deref());
        let Some(binding) = record_binding.and_then(|id| {
            ux.data_bindings
                .iter()
                .find(|binding| binding.binding_id == id)
        }) else {
            self.error(&site.pointer, "UI law: lifecycle transition requires an explicit record binding at its invocation");
            return;
        };
        if record != Some(binding) {
            self.error(
                &site.pointer,
                "UI law: lifecycle transition record binding must match the actual record surface",
            );
            return;
        }
        let Some(Ok(object)) = self.catalog.object_type(&binding.object.id) else {
            self.error(
                &site.pointer,
                "UI law: lifecycle transition record object is unavailable",
            );
            return;
        };
        match generated_transition_enabled_when(root, object, &binding.binding_id) {
            Ok(Some(generated)) => {
                if authored.is_some_and(|condition| {
                    condition.expression.as_str().trim() != generated.expression.as_str()
                        || condition.inputs != generated.inputs
                }) {
                    self.error(&site.pointer, format!("UI law: phase 1 lifecycle enabledWhen must exactly match generated source condition {:?} and inputs; extra predicates are unsupported", generated.expression.as_str()));
                }
            }
            Err(error) => self.error(
                &site.pointer,
                format!("UI law: cannot generate lifecycle enabledWhen: {error}"),
            ),
            Ok(None) => {}
        }
        let Some(ActionEffectProgram::FireTransition {
            object_id_field, ..
        }) = root.effect_program.as_ref()
        else {
            return;
        };
        let mapping = ux
            .action_bindings
            .iter()
            .find(|candidate| candidate.binding_id == binding_id)
            .and_then(|binding| binding.input_mapping.get(object_id_field))
            .map(|expression| expression.as_str().trim());
        match mapping {
            Some("input.recordKey") => {}
            Some(mapping) if mapping.starts_with("input.context.") => self.error(
                &site.pointer,
                "UI law: runtime-key context parameters are not supported for transitions yet",
            ),
            _ => self.error(
                &site.pointer,
                "UI law: lifecycle transition objectIdField must map directly from input.recordKey",
            ),
        }
    }

    pub(super) fn check_generated_transition_condition(&self, binding_id: &str, path: &str) {
        let Some(root) = self.effect_action(self.owner, self.ux, binding_id, path) else {
            return;
        };
        if !matches!(
            root.effect_program,
            Some(ActionEffectProgram::FireTransition { .. })
        ) {
            return;
        }
        let Some(record_id) = self
            .action_bindings
            .get(binding_id)
            .and_then(|binding| binding.inputs.record_binding_id.as_deref())
        else {
            return;
        };
        let Some(data) = self.bindings.get(record_id).copied() else {
            return;
        };
        let Some(Ok(object)) = self.catalog.object_type(&data.object.id) else {
            return;
        };
        match generated_transition_enabled_when(&root, object, record_id) {
            Ok(Some(condition)) => self.check_condition(&condition, path),
            Err(error) => self.error(
                path,
                format!("UI law: cannot generate lifecycle enabledWhen: {error}"),
            ),
            Ok(None) => {}
        }
    }

    fn check_effect_handlers(&self, handlers: &[WidgetEffectHandler], path: &str, shell: bool) {
        self.check_effect_handlers_at(handlers, path, shell, None);
    }

    fn check_effect_handlers_at(
        &self,
        handlers: &[WidgetEffectHandler],
        path: &str,
        shell: bool,
        anchor: Option<&str>,
    ) {
        for (index, handler) in handlers.iter().enumerate() {
            let path = format!("{path}/{index}");
            let pointer = anchor.unwrap_or(&path);
            if shell
                && !matches!(
                    handler,
                    WidgetEffectHandler::ErrorBanner { .. }
                        | WidgetEffectHandler::ResultSurface { .. }
                )
            {
                self.error(
                    pointer,
                    "UI law: shell handlers may handle only exact error and result codes",
                );
            }
            match handler {
                WidgetEffectHandler::ConfirmGuard { confirmation } => self.check_confirmation(
                    confirmation,
                    anchor.unwrap_or(&format!("{path}/confirmation")),
                ),
                WidgetEffectHandler::OutcomeSurface {
                    states,
                    currency_refs,
                } => {
                    if states.is_empty()
                        || states.iter().collect::<BTreeSet<_>>().len() != states.len()
                    {
                        self.error(
                            anchor.unwrap_or(&format!("{path}/states")),
                            "UI law: outcome states must be nonempty and unique",
                        );
                    }
                    self.check_handler_refs(currency_refs, &format!("{path}/currencyRefs"), anchor);
                }
                WidgetEffectHandler::ErrorBanner { codes }
                | WidgetEffectHandler::ResultSurface { codes } => {
                    if codes.is_empty() {
                        self.error(
                            anchor.unwrap_or(&format!("{path}/codes")),
                            "UI law: code handlers must declare at least one exact code",
                        );
                    }
                    self.check_handler_refs(codes, &format!("{path}/codes"), anchor);
                }
            }
        }
    }

    fn check_handler_refs(&self, refs: &[String], path: &str, anchor: Option<&str>) {
        let mut seen = BTreeSet::new();
        for (index, reference) in refs.iter().enumerate() {
            if !valid_effect_ref(reference) || !seen.insert(reference) {
                self.error(anchor.unwrap_or(&format!("{path}/{index}")), "UI law: references must be exact, nonblank, unique, and contain no wildcard, whitespace, or control characters");
            }
        }
    }
}

/// Requirements and provenance of one actual invocation, never of a form opener.
struct EffectInvocation {
    action_id: String,
    action_version: String,
    binding_id: String,
    path: String,
    pointer: String,
    owner_id: String,
    owner_version: String,
    effects: BTreeSet<ActionUiEffect>,
    external: bool,
    currencies: BTreeSet<String>,
    unknown_covered: bool,
}

fn valid_effect_ref(reference: &str) -> bool {
    !reference.is_empty()
        && !reference.chars().any(|character| {
            character.is_whitespace() || character.is_control() || matches!(character, '*' | '?')
        })
}

fn valid_typed_confirmation(confirmation: &WidgetActionConfirmation) -> bool {
    confirmation.strength == norn_ir::WidgetConfirmationStrength::Typed
        && confirmation.phrase.as_ref().is_some_and(|phrase| {
            !phrase.0.is_empty() && phrase.0.values().all(|text| !text.trim().is_empty())
        })
}

fn subtract_confirmation(
    effects: &mut BTreeSet<ActionUiEffect>,
    confirmation: &WidgetActionConfirmation,
) {
    if valid_typed_confirmation(confirmation) {
        effects.remove(&ActionUiEffect::Irreversible);
        effects.remove(&ActionUiEffect::Destructive);
    } else if confirmation.strength == norn_ir::WidgetConfirmationStrength::Click {
        effects.remove(&ActionUiEffect::Destructive);
    }
}

fn complete_unknown_surface(
    handlers: &[WidgetEffectHandler],
    currencies: &BTreeSet<String>,
) -> bool {
    handlers.iter().any(|handler| {
        matches!(handler, WidgetEffectHandler::OutcomeSurface { states, currency_refs } if complete_outcome_states(states) && currencies.iter().all(|currency| currency_refs.contains(currency)))
    })
}

fn complete_outcome_states(states: &[WidgetOutcomeState]) -> bool {
    [
        WidgetOutcomeState::Success,
        WidgetOutcomeState::Failure,
        WidgetOutcomeState::UnknownOutcome,
    ]
    .iter()
    .all(|state| states.contains(state))
}

fn subtract_handlers(
    invocation: &mut EffectInvocation,
    handlers: &[WidgetEffectHandler],
    shell: bool,
) {
    let effects = &mut invocation.effects;
    for handler in handlers {
        match handler {
            WidgetEffectHandler::ConfirmGuard { confirmation } if !shell => {
                subtract_confirmation(effects, confirmation);
            }
            WidgetEffectHandler::OutcomeSurface {
                states,
                currency_refs,
            } if !shell => {
                let complete = complete_outcome_states(states);
                let covers_currencies = invocation
                    .currencies
                    .iter()
                    .all(|currency| currency_refs.contains(currency));
                if complete && covers_currencies {
                    effects.remove(&ActionUiEffect::External);
                }
                if states.contains(&WidgetOutcomeState::Pending) {
                    effects.remove(&ActionUiEffect::Async);
                }
                if states.contains(&WidgetOutcomeState::UnknownOutcome) {
                    effects.remove(&ActionUiEffect::Error {
                        code: "OutcomeUnknown".to_owned(),
                    });
                }
                if covers_currencies && (!invocation.external || complete) {
                    for currency_ref in &invocation.currencies {
                        effects.remove(&ActionUiEffect::Money {
                            currency_ref: currency_ref.clone(),
                        });
                    }
                }
            }
            WidgetEffectHandler::ErrorBanner { codes } => {
                for code in codes.iter().filter(|code| valid_effect_ref(code)) {
                    effects.remove(&ActionUiEffect::Error { code: code.clone() });
                }
            }
            WidgetEffectHandler::ResultSurface { codes } => {
                for code in codes.iter().filter(|code| valid_effect_ref(code)) {
                    effects.remove(&ActionUiEffect::Result { code: code.clone() });
                }
            }
            WidgetEffectHandler::ConfirmGuard { .. }
            | WidgetEffectHandler::OutcomeSurface { .. } => {}
        }
    }
}

fn effect_label(effect: &ActionUiEffect) -> String {
    match effect {
        ActionUiEffect::External => "external".to_owned(),
        ActionUiEffect::Irreversible => "irreversible".to_owned(),
        ActionUiEffect::Destructive => "destructive".to_owned(),
        ActionUiEffect::Money { currency_ref } => format!("money({currency_ref:?})"),
        ActionUiEffect::Async => "async".to_owned(),
        ActionUiEffect::Idempotent => "idempotent".to_owned(),
        ActionUiEffect::Error { code } => format!("error({code:?})"),
        ActionUiEffect::Result { code } => format!("result({code:?})"),
    }
}

struct EffectRenderSite {
    page_id: String,
    path: String,
    pointer: String,
}

impl EffectRenderSite {
    fn child(&self, index: usize) -> Self {
        Self {
            page_id: self.page_id.clone(),
            path: format!("{}/children/{index}", self.path),
            pointer: self.member(&format!("children/{index}")),
        }
    }

    fn composed(&self, page: &str) -> Self {
        Self {
            page_id: self.page_id.clone(),
            path: format!("{}/composed/{page}/root", self.path),
            pointer: self.pointer.clone(),
        }
    }

    fn member(&self, member: &str) -> String {
        if self.path == self.pointer {
            format!("{}/{member}", self.pointer)
        } else {
            self.pointer.clone()
        }
    }
}
