//! Artifact-set semantics for the typed Widget UX contract (#3173).
//!
//! UX references are pinned to exact envelope dependencies. Expressions get
//! only the records named by their `WidgetExpressionInputs`; this checker
//! never gives a widget expression ambient actor or model-link access.
//!
//! The rest of `norn-checker` interacts with this module only through
//! `check_widget_ux_set`, [`generated_transition_enabled_when`],
//! [`GeneratedTransitionEnabledWhenError`], and `value_type`.

mod actions;
mod bindings;
mod child_grid;
mod composition;
mod effects;
mod forms;
mod indexing;
mod routes;
mod tree;
mod types;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use norn_expr::Type;
use norn_expr::schema::Schema;
use norn_ir::{
    ActionRoot, ArtifactDependency, ArtifactEnvelope, ArtifactKind, ArtifactPayload, JsonValue,
    PropertyTypeName, WidgetActionPlacement, WidgetDataBinding, WidgetDataQuery, WidgetForm,
    WidgetPage, WidgetSelectionMode, WidgetUx, WidgetUxActionBinding,
};

use norn_checker::entity_instance::{EffectiveCatalog, EffectiveProperty};
use norn_checker::entity_lowering::entity_object_type;
use norn_checker::entity_scopes::{ScopedViews, scope_visible};
use norn_checker::{
    Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity,
    EntityArtifactSetContext, Span,
};

pub use effects::{GeneratedTransitionEnabledWhenError, generated_transition_enabled_when};
pub(crate) use types::value_type;

const CODE: &str = "widget.validation.invalid_payload";

fn has_initial_lifecycle_default(property: &EffectiveProperty, initial_states: &[String]) -> bool {
    property
        .default
        .as_ref()
        .is_some_and(|default| match &default.0 {
            JsonValue::String(value) => initial_states.iter().any(|state| state == value),
            JsonValue::Null
            | JsonValue::Bool(_)
            | JsonValue::Number(_)
            | JsonValue::Array(_)
            | JsonValue::Object(_) => false,
        })
}

/// Check every candidate widget's typed UX contract against the resolved set.
#[must_use]
pub fn check_widget_ux_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    let combined: Vec<ArtifactEnvelope> = context.base.iter().chain(artifacts).cloned().collect();
    let mut views = ScopedViews::new(artifacts, context.base, context.registry);
    let mut diagnostics = Vec::new();
    for artifact in artifacts {
        let ArtifactPayload::Widget(payload) = &artifact.payload else {
            continue;
        };
        let Some(raw_ux) = payload.ux.as_value() else {
            continue;
        };
        let ux: norn_ir::WidgetUx =
            match serde_json::to_value(raw_ux).and_then(serde_json::from_value) {
                Ok(ux) => ux,
                Err(error) => {
                    diagnostics.push(finding(
                        artifact,
                        "/payload/ux",
                        format!("ux is not a valid typed Widget UX contract: {error}"),
                    ));
                    continue;
                }
            };
        let view = views.of(artifact);
        let mut checker = UxChecker::new(
            artifact,
            &combined,
            view.catalog(),
            context.registry,
            &ux,
            &mut diagnostics,
        );
        checker.check();
        let nullable_inputs = checker.nullable_form_inputs();
        let composed =
            |widget: &norn_ir::WidgetArtifactRef| composed_ux(artifact, &combined, widget);
        diagnostics.extend(crate::ui_interaction::check(
            artifact,
            &ux,
            &nullable_inputs,
            &composed,
        ));
        diagnostics.extend(crate::ui_machine::check(artifact, &ux));
        diagnostics.extend(crate::ui_view::check(artifact, &ux));
        diagnostics.extend(crate::ui_catalog::check(artifact, &ux, &composed));
        let names_lifecycle = ux.pages.iter().any(|page| {
            page.machine
                .as_ref()
                .is_some_and(|machine| machine.lifecycle.is_some())
        });
        if names_lifecycle {
            diagnostics.extend(crate::ui_lifecycle::check(
                artifact,
                &ux,
                view.pool(),
                view.model(),
            ));
        }
    }
    diagnostics
}

struct UxChecker<'a, 'u, 'd> {
    owner: &'a ArtifactEnvelope,
    all: &'a [ArtifactEnvelope],
    catalog: &'a EffectiveCatalog,
    registry: &'a BTreeSet<PropertyTypeName>,
    ux: &'u WidgetUx,
    diagnostics: RefCell<&'d mut Vec<Diagnostic>>,
    pages: BTreeMap<&'u str, &'u WidgetPage>,
    bindings: BTreeMap<&'u str, &'u WidgetDataBinding>,
    action_bindings: BTreeMap<&'u str, &'u WidgetUxActionBinding>,
    actions: BTreeMap<String, ParsedAction>,
    forms: BTreeMap<&'u str, &'u WidgetForm>,
    placements: BTreeMap<&'u str, &'u WidgetActionPlacement>,
    semantic_nodes: BTreeMap<String, SemanticNode>,
    local_semantic_ids: BTreeSet<String>,
    local_field_ids: BTreeSet<String>,
    composed_semantic_ids: BTreeSet<String>,
    included_fields: BTreeSet<(String, String)>,
    included_form_inputs: BTreeSet<(String, String)>,
    presented_form_inputs: BTreeSet<(String, String)>,
    used_field_ids: BTreeSet<String>,
    list_selections: BTreeMap<String, WidgetSelectionMode>,
    child_grid_collections: BTreeSet<(String, String)>,
    list_row_contexts: BTreeSet<String>,
    navigation_uses: BTreeMap<String, Vec<Option<String>>>,
    referenced_placements: BTreeSet<String>,
    page_uses: BTreeMap<Usage, BTreeSet<Option<String>>>,
    rendered_forms: BTreeMap<String, BTreeSet<Option<String>>>,
    rendered_lists: BTreeMap<String, BTreeSet<Option<String>>>,
    action_surfaces: BTreeMap<String, SurfaceContext>,
    usage_surfaces: BTreeMap<Usage, Vec<ActionSurface>>,
    form_owned_semantics: BTreeSet<String>,
    form_input_initialization: BTreeMap<String, FormInputInitialization>,
    composition_stack: BTreeSet<(String, String, String)>,
    next_record: usize,
    enforce_effect_root: bool,
}

#[derive(Clone)]
struct ParsedAction {
    fields: BTreeMap<String, (Schema, bool)>,
    root: ActionRoot,
}

#[derive(Clone)]
struct SemanticNode {
    value_type: Option<Type>,
    option_key_type: Option<Type>,
    control_target: bool,
    action_target: bool,
    form_input_initialization: FormInputInitialization,
}

#[derive(Clone, Default)]
struct FormInputInitialization {
    required: bool,
    source_available: bool,
}

#[derive(Clone)]
struct SurfaceContext {
    surface: ActionSurface,
    binding: Option<WidgetDataBinding>,
    form: Option<WidgetForm>,
    selection: Option<WidgetSelectionMode>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Usage {
    Binding(String),
    Action(String),
    Field(String),
    Form(String),
    Placement(String),
    Navigation(String),
    Semantic(String),
}

#[derive(Clone)]
enum ActionSurface {
    Page,
    List(String),
    Detail(String),
    Form(String),
}

impl<'a, 'u, 'd> UxChecker<'a, 'u, 'd> {
    fn new(
        owner: &'a ArtifactEnvelope,
        all: &'a [ArtifactEnvelope],
        catalog: &'a EffectiveCatalog,
        registry: &'a BTreeSet<PropertyTypeName>,
        ux: &'u WidgetUx,
        diagnostics: &'d mut Vec<Diagnostic>,
    ) -> Self {
        Self {
            owner,
            all,
            catalog,
            registry,
            ux,
            diagnostics: RefCell::new(diagnostics),
            pages: ux
                .pages
                .iter()
                .map(|page| (page.page_id.as_str(), page))
                .collect(),
            bindings: ux
                .data_bindings
                .iter()
                .map(|binding| (binding.binding_id.as_str(), binding))
                .collect(),
            action_bindings: ux
                .action_bindings
                .iter()
                .map(|binding| (binding.binding_id.as_str(), binding))
                .collect(),
            actions: BTreeMap::new(),
            forms: ux
                .forms
                .iter()
                .map(|form| (form.form_id.as_str(), form))
                .collect(),
            placements: ux
                .action_placements
                .iter()
                .map(|placement| (placement.placement_id.as_str(), placement))
                .collect(),
            semantic_nodes: BTreeMap::new(),
            local_semantic_ids: BTreeSet::new(),
            local_field_ids: ux
                .fields
                .iter()
                .map(|field| field.field_id.clone())
                .collect(),
            composed_semantic_ids: BTreeSet::new(),
            included_fields: BTreeSet::new(),
            included_form_inputs: BTreeSet::new(),
            presented_form_inputs: BTreeSet::new(),
            used_field_ids: BTreeSet::new(),
            list_selections: BTreeMap::new(),
            child_grid_collections: BTreeSet::new(),
            list_row_contexts: BTreeSet::new(),
            navigation_uses: BTreeMap::new(),
            referenced_placements: BTreeSet::new(),
            page_uses: BTreeMap::new(),
            rendered_forms: BTreeMap::new(),
            rendered_lists: BTreeMap::new(),
            action_surfaces: BTreeMap::new(),
            usage_surfaces: BTreeMap::new(),
            form_owned_semantics: BTreeSet::new(),
            form_input_initialization: BTreeMap::new(),
            composition_stack: BTreeSet::new(),
            next_record: 0,
            enforce_effect_root: true,
        }
    }

    /// `(form id, input name)` pairs whose every feeding `ActionIR` input leaf is declared nullable.
    /// Only the server's schema decides nullability; an unparsed action or an indirect mapping
    /// contributes nothing, so no clear affordance is ever required by guesswork.
    fn nullable_form_inputs(&self) -> BTreeSet<(String, String)> {
        let mut nullable = BTreeSet::new();
        for form in &self.ux.forms {
            let Some(binding) = self.action_bindings.get(form.submit_binding_id.as_str()) else {
                continue;
            };
            let Some(action) = self.actions.get(binding.binding_id.as_str()) else {
                continue;
            };
            for input_name in &form.input_names {
                let expected = format!("input.draft.{input_name}");
                let fed: Vec<&String> = binding
                    .input_mapping
                    .iter()
                    .filter(|(_, expression)| expression.as_str().trim() == expected)
                    .map(|(action_input, _)| action_input)
                    .collect();
                if !fed.is_empty()
                    && fed.iter().all(|name| {
                        action
                            .fields
                            .get(name.as_str())
                            .is_some_and(|(schema, _)| matches!(schema, Schema::Nullable(_)))
                    })
                {
                    nullable.insert((form.form_id.clone(), input_name.clone()));
                }
            }
        }
        nullable
    }

    fn check(&mut self) {
        self.check_ids();
        self.index_render_uses();
        self.index_override_uses();
        self.propagate_page_uses();
        self.check_binding_record_cycles();
        self.check_bindings();
        self.check_actions();
        self.check_tree_and_fields();
        self.check_forms();
        self.check_placements();
        self.check_navigation_and_app();
        self.check_overrides();
        self.check_effect_laws();
    }

    fn resolve_property(
        &mut self,
        property: &norn_ir::PropertyRef,
        path: &str,
    ) -> Option<&'a EffectiveProperty> {
        let Some(binding) = self.bindings.get(property.binding_id.as_str()).copied() else {
            self.error(
                &format!("{path}/bindingId"),
                "property reference names an unknown data binding",
            );
            return None;
        };
        let Some(entity) = self.find_exact(
            &binding.object.id,
            &binding.object.version,
            &ArtifactKind::Entity,
        ) else {
            self.error(path, "property binding entity does not resolve exactly");
            return None;
        };
        let Some(Ok(object)) = self.catalog.object_type(&entity.artifact_id) else {
            self.error(path, "property binding object type is unavailable");
            return None;
        };
        let Some(property) = object.property(&property.property_id) else {
            self.error(
                &format!("{path}/propertyId"),
                "property id does not exist on the bound object",
            );
            return None;
        };
        Some(property)
    }

    fn entity_object_name(
        &mut self,
        object: &norn_ir::widget_ux::ArtifactRef,
        path: &str,
    ) -> Option<String> {
        let artifact =
            self.resolve_ref(&object.id, &object.version, &ArtifactKind::Entity, path)?;
        let ArtifactPayload::Entity(payload) = &artifact.payload else {
            return None;
        };
        match entity_object_type(payload) {
            Ok(Some(object)) => Some(object.root.object_type.name.as_str().to_owned()),
            Ok(None) => {
                self.error(path, "referenced entity does not define an object type");
                None
            }
            Err(error) => {
                self.error(
                    path,
                    format!("referenced entity cannot be decoded: {error}"),
                );
                None
            }
        }
    }

    fn resolve_ref(
        &mut self,
        id: &str,
        version: &str,
        kind: &ArtifactKind,
        path: &str,
    ) -> Option<&'a ArtifactEnvelope> {
        if !self
            .owner
            .dependencies
            .iter()
            .any(|dependency| dependency_matches(dependency, id, version, kind))
        {
            self.error(path, format!("reference {id:?}@{version:?} must be declared as an exact required envelope dependency"));
            return None;
        }
        let Some(artifact) = self.find_exact(id, version, kind) else {
            self.error(
                path,
                format!("referenced artifact {id:?}@{version:?} does not resolve exactly"),
            );
            return None;
        };
        if !scope_visible(&self.owner.scope, &artifact.scope) {
            self.error(
                path,
                format!(
                    "referenced artifact {id:?}@{version:?} is not visible from this widget's scope"
                ),
            );
            return None;
        }
        Some(artifact)
    }

    fn find_exact(
        &self,
        id: &str,
        version: &str,
        kind: &ArtifactKind,
    ) -> Option<&'a ArtifactEnvelope> {
        self.all.iter().find(|artifact| {
            artifact.artifact_id == id && artifact.version == version && artifact.kind() == *kind
        })
    }

    fn is_bounded_list_binding(&self, id: &str) -> bool {
        self.bindings.get(id).is_some_and(|binding| {
            matches!(
                &binding.query,
                WidgetDataQuery::ObjectList { .. } | WidgetDataQuery::LinkTraversal { .. }
            )
        })
    }

    fn error(&self, path: &str, message: impl Into<String>) {
        self.diagnostics
            .borrow_mut()
            .push(finding(self.owner, path, message));
    }
}

fn finding(owner: &ArtifactEnvelope, path: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        witness: None,
        code: DiagnosticCode::new(CODE),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        span: Some(Span::pointer(path)),
        artifact_id: Some(owner.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

/// The typed UX of `widget` as `owner` may compose it: an exact required
/// dependency of `owner` that resolves in `all` within a scope `owner` sees.
pub(crate) fn composed_ux(
    owner: &ArtifactEnvelope,
    all: &[ArtifactEnvelope],
    widget: &norn_ir::WidgetArtifactRef,
) -> Option<WidgetUx> {
    let kind = ArtifactKind::Widget;
    let pinned = owner
        .dependencies
        .iter()
        .any(|dependency| dependency_matches(dependency, &widget.id, &widget.version, &kind));
    let baseline = all.iter().find(|candidate| {
        candidate.artifact_id == widget.id
            && candidate.version == widget.version
            && candidate.kind() == kind
            && scope_visible(&owner.scope, &candidate.scope)
    });
    let ArtifactPayload::Widget(payload) = &baseline.filter(|_| pinned)?.payload else {
        return None;
    };
    serde_json::to_value(payload.ux.as_value()?)
        .and_then(serde_json::from_value::<WidgetUx>)
        .ok()
}

fn dependency_matches(
    dependency: &ArtifactDependency,
    id: &str,
    version: &str,
    kind: &ArtifactKind,
) -> bool {
    dependency.required
        && dependency.artifact_id == id
        && dependency.version.as_deref() == Some(version)
        && dependency.kind == *kind
}

fn path_index(path: &str, array_prefix: &str) -> Option<usize> {
    path.strip_prefix(array_prefix)?
        .split('/')
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests;
