//! Artifact-set semantics for the typed Widget UX contract (#3173).
//!
//! UX references are pinned to exact envelope dependencies. Expressions get
//! only the records named by their `WidgetExpressionInputs`; this checker
//! never gives a widget expression ambient actor or model-link access.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use norn_expr::schema::Schema;
use norn_expr::{
    ConstructionSource, EnvOptions, Expression, InferredType, Type, ZenTypeEnv, check_expression,
    check_expression_with_sources,
};
use norn_ir::{
    ActionEffectProgram, ActionRoot, ActionUiEffect, ArtifactDependency, ArtifactEnvelope,
    ArtifactKind, ArtifactPayload, JsonValue, LocaleText, PropertyType, WidgetActionConfirmation,
    WidgetActionLocation, WidgetActionPlacement, WidgetActionSeverity, WidgetComponent,
    WidgetDataBinding, WidgetDataQuery, WidgetEffectHandler, WidgetExpressionInputs,
    WidgetFieldControl, WidgetFieldFormat, WidgetFieldTarget, WidgetFilter, WidgetFilterOperator,
    WidgetFilterValue, WidgetForm, WidgetFormMode, WidgetNavigationDestination, WidgetNode,
    WidgetOutcomeState, WidgetOverrideValue, WidgetPage, WidgetSelectionMode, WidgetSort, WidgetUx,
    WidgetUxActionBinding, ZenCondition, ZenExpression,
};

use crate::entity_instance::{
    EffectiveCatalog, EffectiveKind, EffectiveObjectType, EffectiveProperty,
};
use crate::entity_lowering::entity_object_type;
use crate::graph::{cycle_groups, topological_order};
use crate::{
    Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity,
    EntityArtifactSetContext, Span,
};

const CODE: &str = "widget.validation.invalid_payload";
// Mirrors `norn-projections::sqlite_ddl::MAX_COLUMNS_PER_TABLE`. The checker
// cannot depend on the projection crate, but must reject query properties that
// would be placed in the table's overflow JSON column instead of an indexable
// physical column.
const MAX_SQLITE_COLUMNS_PER_TABLE: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingColumnError {
    Computed,
    Overflow,
    NotDeclared,
    Unavailable,
}

fn is_binding_index_sort_type(property: &EffectiveProperty) -> bool {
    matches!(
        &property.kind,
        EffectiveKind::Base(
            PropertyType::Text { .. }
                | PropertyType::Enum { .. }
                | PropertyType::Integer {}
                | PropertyType::Boolean {}
                | PropertyType::Date {}
        )
    )
}

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

/// Check every candidate widget's typed UX contract against the resolved set.
pub(crate) fn check_widget_ux_set(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    let combined: Vec<ArtifactEnvelope> = context.base.iter().chain(artifacts).cloned().collect();
    let catalog = EffectiveCatalog::from_artifacts_with_registry(&combined, context.registry);
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
        let mut checker = UxChecker::new(artifact, &combined, &catalog, &ux, &mut diagnostics);
        checker.check();
        let nullable_inputs = checker.nullable_form_inputs();
        let composed = |widget: &norn_ir::WidgetArtifactRef| {
            let baseline = combined.iter().find(|candidate| {
                candidate.artifact_id == widget.id
                    && candidate.version == widget.version
                    && candidate.kind() == ArtifactKind::Widget
            })?;
            let ArtifactPayload::Widget(payload) = &baseline.payload else {
                return None;
            };
            serde_json::to_value(payload.ux.as_value()?)
                .and_then(serde_json::from_value::<norn_ir::WidgetUx>)
                .ok()
        };
        diagnostics.extend(crate::ui_interaction::check(
            artifact,
            &ux,
            &nullable_inputs,
            &composed,
        ));
    }
    diagnostics
}

struct UxChecker<'a, 'u, 'd> {
    owner: &'a ArtifactEnvelope,
    all: &'a [ArtifactEnvelope],
    catalog: &'a EffectiveCatalog,
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
        ux: &'u WidgetUx,
        diagnostics: &'d mut Vec<Diagnostic>,
    ) -> Self {
        Self {
            owner,
            all,
            catalog,
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

    fn index_render_uses(&mut self) {
        if let Some(app) = &self.ux.app {
            for id in &app.navigation_item_ids {
                self.page_uses
                    .entry(Usage::Navigation(id.clone()))
                    .or_default()
                    .insert(None);
            }
        }
        for (index, page) in self.ux.pages.clone().iter().enumerate() {
            self.index_node_uses(
                &page.root,
                &page.page_id,
                &ActionSurface::Page,
                &format!("/payload/ux/pages/{index}/root"),
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "each closed component indexes its page, form, binding, and field uses together"
    )]
    fn index_node_uses(
        &mut self,
        node: &WidgetNode,
        page_id: &str,
        surface: &ActionSurface,
        path: &str,
    ) {
        let form_owned = self.form_owned_node(node, surface);
        if let Some(id) = &node.semantic_id {
            self.add_page_use(Usage::Semantic(id.clone()), page_id);
            self.add_surface_use(Usage::Semantic(id.clone()), surface);
            if form_owned {
                self.form_owned_semantics.insert(id.clone());
            }
            if matches!(&node.component, WidgetComponent::Action { .. }) {
                self.action_surfaces
                    .insert(id.clone(), self.surface_context(surface));
            }
        }
        let child_surface = match &node.component {
            WidgetComponent::Section { section } => {
                for property in &section.fields {
                    self.index_property_use(property, page_id, surface);
                }
                surface.clone()
            }
            WidgetComponent::List {
                data_binding_id,
                fields,
                ..
            } => {
                self.add_page_use(Usage::Binding(data_binding_id.clone()), page_id);
                self.rendered_lists
                    .entry(data_binding_id.clone())
                    .or_default()
                    .insert(Some(page_id.to_owned()));
                for property in fields {
                    self.index_property_use(
                        property,
                        page_id,
                        &ActionSurface::List(data_binding_id.clone()),
                    );
                }
                ActionSurface::List(data_binding_id.clone())
            }
            WidgetComponent::Detail { data_binding_id } => {
                self.add_page_use(Usage::Binding(data_binding_id.clone()), page_id);
                ActionSurface::Detail(data_binding_id.clone())
            }
            WidgetComponent::ChildGrid {
                form_id,
                data_binding_id,
                field_ids,
                ..
            } => {
                if let Some(binding_id) = data_binding_id {
                    self.add_page_use(Usage::Binding(binding_id.clone()), page_id);
                    self.rendered_lists
                        .entry(binding_id.clone())
                        .or_default()
                        .insert(Some(page_id.to_owned()));
                }
                for field_id in field_ids {
                    self.add_page_use(Usage::Field(field_id.clone()), page_id);
                    self.add_page_use(Usage::Semantic(field_id.clone()), page_id);
                    if let Some(binding_id) = data_binding_id {
                        self.add_surface_use(
                            Usage::Field(field_id.clone()),
                            &ActionSurface::List(binding_id.clone()),
                        );
                        self.add_surface_use(
                            Usage::Semantic(field_id.clone()),
                            &ActionSurface::List(binding_id.clone()),
                        );
                    } else {
                        self.add_surface_use(Usage::Field(field_id.clone()), surface);
                        self.add_surface_use(Usage::Semantic(field_id.clone()), surface);
                    }
                    self.form_owned_semantics.insert(field_id.clone());
                }
                ActionSurface::Form(form_id.clone())
            }
            WidgetComponent::Dialog { .. } => surface.clone(),
            WidgetComponent::Form { form_id } => {
                self.add_page_use(Usage::Form(form_id.clone()), page_id);
                self.rendered_forms
                    .entry(form_id.clone())
                    .or_default()
                    .insert(Some(page_id.to_owned()));
                ActionSurface::Form(form_id.clone())
            }
            WidgetComponent::Field { field_id } => {
                self.add_page_use(Usage::Field(field_id.clone()), page_id);
                self.add_page_use(Usage::Semantic(field_id.clone()), page_id);
                self.add_surface_use(Usage::Field(field_id.clone()), surface);
                self.add_surface_use(Usage::Semantic(field_id.clone()), surface);
                if form_owned {
                    self.form_owned_semantics.insert(field_id.clone());
                }
                surface.clone()
            }
            WidgetComponent::Action { placement_id } => {
                self.add_page_use(Usage::Placement(placement_id.clone()), page_id);
                self.add_surface_use(Usage::Placement(placement_id.clone()), surface);
                surface.clone()
            }
            WidgetComponent::Navigation { item_id } => {
                self.add_page_use(Usage::Navigation(item_id.clone()), page_id);
                self.add_surface_use(Usage::Navigation(item_id.clone()), surface);
                surface.clone()
            }
            WidgetComponent::Composition {
                widget,
                page_id: source_page,
            } => {
                self.check_composition(widget, source_page, page_id, &format!("{path}/component"));
                surface.clone()
            }
        };
        for (index, child) in node.children.iter().enumerate() {
            self.index_node_uses(
                child,
                page_id,
                &child_surface,
                &format!("{path}/children/{index}"),
            );
        }
    }

    fn surface_context(&self, surface: &ActionSurface) -> SurfaceContext {
        let binding = match surface {
            ActionSurface::List(id) | ActionSurface::Detail(id) => {
                self.bindings.get(id.as_str()).copied().cloned()
            }
            ActionSurface::Page | ActionSurface::Form(_) => None,
        };
        let form = match surface {
            ActionSurface::Form(id) => self.forms.get(id.as_str()).copied().cloned(),
            ActionSurface::Page | ActionSurface::List(_) | ActionSurface::Detail(_) => None,
        };
        let selection = match surface {
            ActionSurface::List(id) => self.list_selections.get(id).copied(),
            ActionSurface::Page | ActionSurface::Detail(_) | ActionSurface::Form(_) => None,
        };
        SurfaceContext {
            surface: surface.clone(),
            binding,
            form,
            selection,
        }
    }

    fn form_owned_node(&self, node: &WidgetNode, surface: &ActionSurface) -> bool {
        matches!(surface, ActionSurface::Form(_))
            || match &node.component {
                WidgetComponent::Form { .. } => true,
                WidgetComponent::Field { field_id } => self.ux.fields.iter().any(|field| {
                    field.field_id == *field_id
                        && matches!(
                            &field.target,
                            WidgetFieldTarget::FormInput { .. }
                                | WidgetFieldTarget::ParentProperty { .. }
                        )
                }),
                WidgetComponent::Section { .. }
                | WidgetComponent::List { .. }
                | WidgetComponent::Detail { .. }
                | WidgetComponent::Dialog { .. }
                | WidgetComponent::ChildGrid { .. }
                | WidgetComponent::Action { .. }
                | WidgetComponent::Navigation { .. }
                | WidgetComponent::Composition { .. } => false,
            }
    }

    fn add_page_use(&mut self, usage: Usage, page_id: &str) {
        self.page_uses
            .entry(usage)
            .or_default()
            .insert(Some(page_id.to_owned()));
    }

    fn add_surface_use(&mut self, usage: Usage, surface: &ActionSurface) {
        self.usage_surfaces
            .entry(usage)
            .or_default()
            .push(surface.clone());
    }

    fn index_property_use(
        &mut self,
        property: &norn_ir::PropertyRef,
        page_id: &str,
        surface: &ActionSurface,
    ) {
        self.add_page_use(Usage::Binding(property.binding_id.clone()), page_id);
        let field_ids: Vec<String> = self
            .ux
            .fields
            .iter()
            .filter(|field| {
                matches!(&field.target, WidgetFieldTarget::Property { property: target } if target == property)
            })
            .map(|field| field.field_id.clone())
            .collect();
        for id in field_ids {
            self.add_page_use(Usage::Field(id.clone()), page_id);
            self.add_page_use(Usage::Semantic(id.clone()), page_id);
            self.add_surface_use(Usage::Field(id.clone()), surface);
            self.add_surface_use(Usage::Semantic(id), surface);
        }
    }

    fn index_override_uses(&mut self) {
        for (index, override_) in self.ux.overrides.clone().iter().enumerate() {
            let WidgetOverrideValue::ActionPlacement { placement_id } = &override_.value else {
                continue;
            };
            if !self
                .composed_semantic_ids
                .contains(&override_.target_semantic_id)
            {
                continue;
            }
            let Some(surface) = self
                .action_surfaces
                .get(&override_.target_semantic_id)
                .cloned()
            else {
                continue;
            };
            let Some(placement) = self.placements.get(placement_id.as_str()).copied() else {
                continue;
            };
            let path = format!("/payload/ux/overrides/{index}/value/placementId");
            if let Some(binding) = &surface.binding {
                if self.bindings.get(binding.binding_id.as_str()).copied() != Some(binding) {
                    self.error(&path, "action placement override must preserve the imported surface's data binding");
                } else if let Some(selection) = surface.selection
                    && self
                        .list_selections
                        .get(&binding.binding_id)
                        .is_some_and(|existing| *existing != selection)
                {
                    self.error(
                        &path,
                        "action placement override cannot change the imported list selection mode",
                    );
                }
            }
            if let Some(form) = &surface.form
                && self.forms.get(form.form_id.as_str()).copied() != Some(form)
            {
                self.error(
                    &path,
                    "action placement override must preserve the imported surface's form",
                );
            }
            self.check_action_surface(placement, &surface.surface, &path);
            self.add_surface_use(Usage::Placement(placement_id.clone()), &surface.surface);
            self.referenced_placements.insert(placement_id.clone());
            let pages = self
                .page_uses
                .get(&Usage::Semantic(override_.target_semantic_id.clone()))
                .cloned()
                .unwrap_or_default();
            self.page_uses
                .entry(Usage::Placement(placement_id.clone()))
                .or_default()
                .extend(pages);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Every typed reference propagates its caller's pages to the referenced definition."
    )]
    fn propagate_page_uses(&mut self) {
        let mut edges = Vec::new();
        for (index, binding) in self.ux.data_bindings.iter().enumerate() {
            let usage = Usage::Binding(binding.binding_id.clone());
            for (source_id, _) in binding_record_dependencies(binding, index) {
                edges.push((usage.clone(), Usage::Binding(source_id.to_owned())));
            }
        }
        for binding in &self.ux.action_bindings {
            add_input_uses(
                &mut edges,
                &Usage::Action(binding.binding_id.clone()),
                &binding.inputs,
            );
        }
        for field in &self.ux.fields {
            let usage = Usage::Field(field.field_id.clone());
            match &field.target {
                WidgetFieldTarget::Property { property } => {
                    edges.push((usage.clone(), Usage::Binding(property.binding_id.clone())));
                }
                WidgetFieldTarget::FormInput {
                    form_id, initial, ..
                } => {
                    edges.push((usage.clone(), Usage::Form(form_id.clone())));
                    if let Some(property) = initial {
                        edges.push((usage.clone(), Usage::Binding(property.binding_id.clone())));
                    }
                }
                WidgetFieldTarget::ParentProperty { form_id, .. }
                | WidgetFieldTarget::ChildRowProperty { form_id, .. } => {
                    edges.push((usage.clone(), Usage::Form(form_id.clone())));
                }
            }
            if let Some(id) = &field.option_query_binding_id {
                edges.push((usage.clone(), Usage::Binding(id.clone())));
            }
            for condition in [
                &field.visible_when,
                &field.required_when,
                &field.read_only_when,
            ]
            .into_iter()
            .flatten()
            {
                add_input_uses(&mut edges, &usage, &condition.inputs);
            }
        }
        for form in &self.ux.forms {
            let usage = Usage::Form(form.form_id.clone());
            edges.push((usage.clone(), Usage::Action(form.submit_binding_id.clone())));
            if let Some(id) = &form.data_binding_id {
                edges.push((usage, Usage::Binding(id.clone())));
            }
        }
        for placement in &self.ux.action_placements {
            let usage = Usage::Placement(placement.placement_id.clone());
            edges.push((usage.clone(), Usage::Action(placement.binding_id.clone())));
            if let Some(id) = &placement.form_id {
                edges.push((usage.clone(), Usage::Form(id.clone())));
            }
            if let Some(id) = &placement.selection_binding_id {
                edges.push((usage.clone(), Usage::Binding(id.clone())));
            }
            if let Some(condition) = &placement.enabled_when {
                add_input_uses(&mut edges, &usage, &condition.inputs);
            }
        }
        for item in &self.ux.navigation_items {
            let usage = Usage::Navigation(item.item_id.clone());
            if let Some(condition) = &item.visible_when {
                add_input_uses(&mut edges, &usage, &condition.inputs);
            }
            match &item.destination {
                WidgetNavigationDestination::Page { parameters, .. } => {
                    for value in parameters.values() {
                        add_value_use(&mut edges, &usage, value);
                    }
                }
                WidgetNavigationDestination::Object {
                    data_binding_id, ..
                }
                | WidgetNavigationDestination::NativeProjection {
                    data_binding_id, ..
                } => {
                    edges.push((usage, Usage::Binding(data_binding_id.clone())));
                }
            }
        }
        for override_ in &self.ux.overrides {
            if let WidgetOverrideValue::Visibility { condition } = &override_.value {
                add_input_uses(
                    &mut edges,
                    &Usage::Semantic(override_.target_semantic_id.clone()),
                    &condition.inputs,
                );
            }
        }
        loop {
            let mut changed = false;
            for (source, target) in &edges {
                let pages = self.page_uses.get(source).cloned().unwrap_or_default();
                let target_pages = self.page_uses.entry(target.clone()).or_default();
                let before = target_pages.len();
                target_pages.extend(pages);
                changed |= before != target_pages.len();
            }
            if !changed {
                break;
            }
        }
    }

    fn usage_at(&self, path: &str) -> Option<Usage> {
        path_index(path, "/payload/ux/dataBindings/")
            .and_then(|index| self.ux.data_bindings.get(index))
            .map(|binding| Usage::Binding(binding.binding_id.clone()))
            .or_else(|| {
                path_index(path, "/payload/ux/actionBindings/")
                    .and_then(|index| self.ux.action_bindings.get(index))
                    .map(|binding| Usage::Action(binding.binding_id.clone()))
            })
            .or_else(|| {
                path_index(path, "/payload/ux/fields/")
                    .and_then(|index| self.ux.fields.get(index))
                    .map(|field| Usage::Field(field.field_id.clone()))
            })
            .or_else(|| {
                path_index(path, "/payload/ux/forms/")
                    .and_then(|index| self.ux.forms.get(index))
                    .map(|form| Usage::Form(form.form_id.clone()))
            })
            .or_else(|| {
                path_index(path, "/payload/ux/actionPlacements/")
                    .and_then(|index| self.ux.action_placements.get(index))
                    .map(|placement| Usage::Placement(placement.placement_id.clone()))
            })
            .or_else(|| {
                path_index(path, "/payload/ux/navigationItems/")
                    .and_then(|index| self.ux.navigation_items.get(index))
                    .map(|item| Usage::Navigation(item.item_id.clone()))
            })
            .or_else(|| {
                path_index(path, "/payload/ux/overrides/")
                    .and_then(|index| self.ux.overrides.get(index))
                    .map(|override_| Usage::Semantic(override_.target_semantic_id.clone()))
            })
    }

    fn pages_at(&self, path: &str) -> BTreeSet<Option<String>> {
        self.usage_at(path)
            .and_then(|usage| self.page_uses.get(&usage))
            .cloned()
            .unwrap_or_default()
    }

    fn check_context_page_at(&self, page_id: &str, path: &str) -> bool {
        let pages = self.pages_at(path);
        let available =
            !pages.is_empty() && pages.iter().all(|page| page.as_deref() == Some(page_id));
        if !available {
            self.error(path, "contextPageId must match every rendering or invocation page; context is unavailable at this use");
        }
        available
    }

    fn check_effect_laws(&self) {
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
                self.owner,
                self.ux,
                &page.root,
                &site,
                &[],
                None,
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
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit lexical scope and render provenance for composed trees"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "postorder expansion preserves lexical handler scope and invocation provenance"
    )]
    fn effect_tree(
        &self,
        owner: &ArtifactEnvelope,
        ux: &WidgetUx,
        node: &WidgetNode,
        site: &EffectRenderSite,
        overrides: &[(&ArtifactEnvelope, &WidgetUx)],
        record: Option<&WidgetDataBinding>,
        stack: &mut BTreeSet<(String, String, String)>,
    ) -> Vec<EffectInvocation> {
        let path = &site.path;
        let record = match &node.component {
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
            | WidgetComponent::Composition { .. } => record,
        };
        let mut invocations = Vec::new();
        match &node.component {
            WidgetComponent::Action { placement_id } => {
                let replacement = node.semantic_id.as_ref().and_then(|id| {
                    overrides.iter().find_map(|(scope_owner, scope_ux)| {
                        scope_ux.overrides.iter().find_map(|override_| {
                            if override_.target_semantic_id != *id {
                                return None;
                            }
                            let WidgetOverrideValue::ActionPlacement { placement_id } =
                                &override_.value
                            else {
                                return None;
                            };
                            Some((*scope_owner, *scope_ux, placement_id.as_str()))
                        })
                    })
                });
                let (placement_owner, placement_ux, id) =
                    replacement.unwrap_or((owner, ux, placement_id.as_str()));
                if let Some(placement) = placement_ux
                    .action_placements
                    .iter()
                    .find(|placement| placement.placement_id == id)
                {
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
                            self.error(&site.pointer, "UI law: a form opener cannot expose retry because it invokes no action");
                        }
                    } else if let Some(root) = root.as_ref()
                        && let Some(mut invocation) = Self::effect_invocation(
                            placement_owner,
                            placement_ux,
                            &placement.binding_id,
                            root,
                            site,
                        )
                    {
                        if placement.retry
                            && !invocation.effects.contains(&ActionUiEffect::Idempotent)
                        {
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
                        invocations.push(invocation);
                    }
                }
            }
            WidgetComponent::Form { form_id } => {
                if let Some(form) = ux.forms.iter().find(|form| form.form_id == *form_id) {
                    let handler_path = ux
                        .forms
                        .iter()
                        .position(|candidate| candidate.form_id == *form_id)
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
                    let root =
                        self.effect_action(owner, ux, &form.submit_binding_id, &site.pointer);
                    if let Some(root) = root.as_ref() {
                        self.check_transition_condition(
                            root,
                            ux,
                            &form.submit_binding_id,
                            None,
                            record,
                            site,
                        );
                    }
                    if let Some(root) = root.as_ref()
                        && let Some(mut invocation) =
                            Self::effect_invocation(owner, ux, &form.submit_binding_id, root, site)
                    {
                        invocation.effects.remove(&ActionUiEffect::Idempotent);
                        subtract_confirmation(&mut invocation.effects, &form.submit.confirmation);
                        invocation.unknown_covered =
                            complete_unknown_surface(&form.submit.handlers, &invocation.currencies);
                        subtract_handlers(&mut invocation, &form.submit.handlers, false);
                        invocations.push(invocation);
                    }
                }
            }
            WidgetComponent::Composition { widget, page_id } => {
                if let Some(artifact) =
                    self.find_exact(&widget.id, &widget.version, &ArtifactKind::Widget)
                    && let ArtifactPayload::Widget(payload) = &artifact.payload
                    && let Some(raw) = payload.ux.as_value()
                    && let Ok(composed) =
                        serde_json::to_value(raw).and_then(serde_json::from_value::<WidgetUx>)
                    && let Some(page) = composed.pages.iter().find(|page| page.page_id == *page_id)
                {
                    let identity = (
                        artifact.artifact_id.clone(),
                        artifact.version.clone(),
                        page_id.clone(),
                    );
                    if stack.insert(identity.clone()) {
                        let mut scopes = overrides.to_vec();
                        scopes.push((owner, ux));
                        invocations.extend(self.effect_tree(
                            artifact,
                            &composed,
                            &page.root,
                            &site.composed(page_id),
                            &scopes,
                            record,
                            stack,
                        ));
                        stack.remove(&identity);
                    }
                }
            }
            WidgetComponent::Section { .. }
            | WidgetComponent::List { .. }
            | WidgetComponent::Detail { .. }
            | WidgetComponent::Dialog { .. }
            | WidgetComponent::ChildGrid { .. }
            | WidgetComponent::Field { .. }
            | WidgetComponent::Navigation { .. } => {}
        }
        for (index, child) in node.children.iter().enumerate() {
            invocations.extend(self.effect_tree(
                owner,
                ux,
                child,
                &site.child(index),
                overrides,
                record,
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
            if !external.is_empty() && !*blocking {
                self.error(
                    &site.member("component/blocking"),
                    "UI law: a dialog containing an external invocation must be blocking",
                );
            }
            if *blocking {
                if external.is_empty()
                    || external.iter().any(|invocation| {
                        binding_id.as_deref() != Some(invocation.binding_id.as_str())
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
        invocations
    }

    fn effect_action(
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

    fn check_generated_transition_condition(&mut self, binding_id: &str, path: &str) {
        let Some(root) = self.effect_action(self.owner, self.ux, binding_id, path) else {
            return;
        };
        if !matches!(
            root.effect_program,
            Some(ActionEffectProgram::FireTransition { .. })
        ) {
            return;
        }
        let binding = self.action_bindings.get(binding_id).copied().cloned();
        let Some(record_id) = binding
            .as_ref()
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

    #[expect(
        clippy::too_many_lines,
        reason = "This pass validates stable ids and cross-references across the flat UX contract collections."
    )]
    fn check_ids(&mut self) {
        let app = self.ux.app.clone();
        let pages = self.ux.pages.clone();
        let data_bindings = self.ux.data_bindings.clone();
        let action_bindings = self.ux.action_bindings.clone();
        let fields = self.ux.fields.clone();
        let forms = self.ux.forms.clone();
        let placements = self.ux.action_placements.clone();
        let navigation_items = self.ux.navigation_items.clone();
        let overrides = self.ux.overrides.clone();
        if let Some(app) = &app {
            self.check_global_id(&mut BTreeSet::new(), &app.app_id, "/payload/ux/app/appId");
        }
        let mut page_ids = BTreeSet::new();
        for (index, page) in pages.iter().enumerate() {
            self.check_global_id(
                &mut page_ids,
                &page.page_id,
                &format!("/payload/ux/pages/{index}/pageId"),
            );
            if page.name.trim().is_empty() {
                self.error(
                    &format!("/payload/ux/pages/{index}/name"),
                    "page name must not be blank",
                );
            }
            let mut params = BTreeSet::new();
            for (param_index, param) in page.context_params.iter().enumerate() {
                let param_path = format!("/payload/ux/pages/{index}/contextParams/{param_index}");
                if param.param_id.trim().is_empty() || !params.insert(param.param_id.as_str()) {
                    self.error(
                        &format!("{param_path}/paramId"),
                        "context parameter ids must be nonblank and unique within a page",
                    );
                }
                if let Some(runtime_key_of) = &param.runtime_key_of {
                    if !param.required
                        || param.value_type != norn_ir::widget_ux::WidgetValueType::Text
                    {
                        self.error(
                            &format!("{param_path}/runtimeKeyOf"),
                            "runtimeKeyOf requires a required text context parameter",
                        );
                    }
                    self.resolve_ref(
                        &runtime_key_of.id,
                        &runtime_key_of.version,
                        &ArtifactKind::Entity,
                        &format!("{param_path}/runtimeKeyOf"),
                    );
                }
            }
        }
        let mut data_binding_ids = BTreeSet::new();
        for (index, binding) in data_bindings.iter().enumerate() {
            self.check_global_id(
                &mut data_binding_ids,
                &binding.binding_id,
                &format!("/payload/ux/dataBindings/{index}/bindingId"),
            );
        }
        let mut action_binding_ids = BTreeSet::new();
        for (index, binding) in action_bindings.iter().enumerate() {
            self.check_global_id(
                &mut action_binding_ids,
                &binding.binding_id,
                &format!("/payload/ux/actionBindings/{index}/bindingId"),
            );
        }
        let mut field_ids = BTreeSet::new();
        for (index, field) in fields.iter().enumerate() {
            self.check_global_id(
                &mut field_ids,
                &field.field_id,
                &format!("/payload/ux/fields/{index}/fieldId"),
            );
        }
        let mut form_ids = BTreeSet::new();
        for (index, form) in forms.iter().enumerate() {
            self.check_global_id(
                &mut form_ids,
                &form.form_id,
                &format!("/payload/ux/forms/{index}/formId"),
            );
        }
        let mut placement_ids = BTreeSet::new();
        for (index, placement) in placements.iter().enumerate() {
            self.check_global_id(
                &mut placement_ids,
                &placement.placement_id,
                &format!("/payload/ux/actionPlacements/{index}/placementId"),
            );
        }
        let mut navigation_item_ids = BTreeSet::new();
        for (index, item) in navigation_items.iter().enumerate() {
            self.check_global_id(
                &mut navigation_item_ids,
                &item.item_id,
                &format!("/payload/ux/navigationItems/{index}/itemId"),
            );
        }
        if let Some(app) = &app {
            let mut route_ids = BTreeSet::new();
            for (index, route) in app.routes.iter().enumerate() {
                self.check_global_id(
                    &mut route_ids,
                    &route.route_id,
                    &format!("/payload/ux/app/routes/{index}/routeId"),
                );
            }
        }
        let mut semantic_ids = BTreeSet::new();
        let mut node_ids = BTreeSet::new();
        let mut section_ids = BTreeSet::new();
        if let Some(app) = &app {
            for item_id in &app.navigation_item_ids {
                self.navigation_uses
                    .entry(item_id.clone())
                    .or_default()
                    .push(None);
            }
        }
        for (page_index, page) in pages.iter().enumerate() {
            self.check_node_ids(
                &page.root,
                &format!("/payload/ux/pages/{page_index}/root"),
                &mut node_ids,
                &mut section_ids,
                &mut semantic_ids,
                None,
            );
        }
        for (index, override_) in overrides.iter().enumerate() {
            if override_.target_semantic_id.trim().is_empty() {
                self.error(
                    &format!("/payload/ux/overrides/{index}/targetSemanticId"),
                    "override target id must not be blank",
                );
            }
        }
    }

    fn check_global_id(&mut self, ids: &mut BTreeSet<String>, id: &str, path: &str) {
        if id.trim().is_empty() || !ids.insert(id.to_owned()) {
            self.error(
                path,
                "stable ids must be nonblank and unique across Widget UX",
            );
        }
    }

    fn check_node_ids(
        &mut self,
        node: &WidgetNode,
        path: &str,
        node_ids: &mut BTreeSet<String>,
        section_ids: &mut BTreeSet<String>,
        semantic_ids: &mut BTreeSet<String>,
        row_context: Option<&str>,
    ) {
        self.check_global_id(node_ids, &node.node_id, &format!("{path}/nodeId"));
        let child_row_context = match &node.component {
            WidgetComponent::List {
                data_binding_id,
                selection,
                ..
            } => {
                if let Some(existing) = self.list_selections.get(data_binding_id)
                    && existing != selection
                {
                    self.error(
                        &format!("{path}/component/selection"),
                        "one data binding cannot use conflicting selection modes",
                    );
                }
                self.list_selections
                    .insert(data_binding_id.clone(), *selection);
                self.list_row_contexts.insert(data_binding_id.clone());
                Some(data_binding_id.as_str())
            }
            WidgetComponent::Detail { data_binding_id } => Some(data_binding_id.as_str()),
            WidgetComponent::ChildGrid {
                data_binding_id: Some(data_binding_id),
                ..
            } => {
                self.list_row_contexts.insert(data_binding_id.clone());
                Some(data_binding_id.as_str())
            }
            WidgetComponent::Section { .. }
            | WidgetComponent::Dialog { .. }
            | WidgetComponent::Form { .. }
            | WidgetComponent::ChildGrid { .. }
            | WidgetComponent::Field { .. }
            | WidgetComponent::Action { .. }
            | WidgetComponent::Navigation { .. }
            | WidgetComponent::Composition { .. } => row_context,
        };
        if let WidgetComponent::Navigation { item_id } = &node.component {
            self.navigation_uses
                .entry(item_id.clone())
                .or_default()
                .push(row_context.map(str::to_owned));
        }
        if let WidgetComponent::Section { section } = &node.component {
            self.check_global_id(
                section_ids,
                &section.section_id,
                &format!("{path}/component/section/sectionId"),
            );
        }
        if let Some(semantic_id) = &node.semantic_id {
            if semantic_id.trim().is_empty()
                || !semantic_ids.insert(semantic_id.clone())
                || self.local_field_ids.contains(semantic_id)
            {
                self.error(
                    &format!("{path}/semanticId"),
                    "semantic ids must be nonblank and unique across Widget UX pages",
                );
            } else if self.semantic_nodes.contains_key(semantic_id) {
                self.error(
                    &format!("{path}/semanticId"),
                    "semantic id collides with an imported component",
                );
            } else {
                self.local_semantic_ids.insert(semantic_id.clone());
                self.semantic_nodes.insert(
                    semantic_id.clone(),
                    SemanticNode {
                        value_type: None,
                        option_key_type: None,
                        control_target: matches!(&node.component, WidgetComponent::Field { .. }),
                        action_target: matches!(&node.component, WidgetComponent::Action { .. }),
                        form_input_initialization: FormInputInitialization::default(),
                    },
                );
            }
        }
        for (index, child) in node.children.iter().enumerate() {
            self.check_node_ids(
                child,
                &format!("{path}/children/{index}"),
                node_ids,
                section_ids,
                semantic_ids,
                child_row_context,
            );
        }
    }

    fn check_bindings(&mut self) {
        let bindings = self.ux.data_bindings.clone();
        for (index, binding) in bindings.iter().enumerate() {
            let path = format!("/payload/ux/dataBindings/{index}");
            let Some(entity) = self.resolve_ref(
                &binding.object.id,
                &binding.object.version,
                &ArtifactKind::Entity,
                &format!("{path}/object"),
            ) else {
                continue;
            };
            let ArtifactPayload::Entity(payload) = &entity.payload else {
                continue;
            };
            let object = match entity_object_type(payload) {
                Ok(Some(object)) => object,
                Ok(None) => {
                    self.error(
                        &format!("{path}/object"),
                        "binding artifact does not define an object type",
                    );
                    continue;
                }
                Err(error) => {
                    self.error(
                        &format!("{path}/object"),
                        format!("binding entity cannot be decoded: {error}"),
                    );
                    continue;
                }
            };
            let type_name = object.root.object_type.name.as_str();
            let Some(Ok(effective)) = self.catalog.object_type(&entity.artifact_id) else {
                self.error(
                    &format!("{path}/object"),
                    "binding object type is not available in the effective entity catalog",
                );
                continue;
            };
            let effective = effective.clone();
            if effective.name.as_str() != type_name {
                self.error(
                    &format!("{path}/object"),
                    "binding artifact does not own the resolved object type",
                );
                continue;
            }
            self.check_data_query(binding, &effective, index, &path);
        }
    }

    fn check_binding_record_cycles(&self) {
        let bindings = &self.ux.data_bindings;
        let indices: BTreeMap<&str, usize> = bindings
            .iter()
            .enumerate()
            .map(|(index, binding)| (binding.binding_id.as_str(), index))
            .collect();
        let mut requires: BTreeMap<usize, BTreeSet<usize>> = (0..bindings.len())
            .map(|index| (index, BTreeSet::new()))
            .collect();
        let mut references = Vec::new();
        for (index, binding) in bindings.iter().enumerate() {
            for (source_id, pointer) in binding_record_dependencies(binding, index) {
                if let Some(&source_index) = indices.get(source_id) {
                    requires.entry(index).or_default().insert(source_index);
                    references.push((index, source_index, pointer));
                }
            }
        }
        let (_, blocked) = topological_order(&requires, |index| index);
        for group in cycle_groups(&requires, &blocked) {
            let Some((_, _, pointer)) = references
                .iter()
                .find(|(from, to, _)| group.contains(from) && group.contains(to))
            else {
                continue;
            };
            let names: Vec<&str> = group
                .iter()
                .filter_map(|&index| bindings.get(index))
                .map(|binding| binding.binding_id.as_str())
                .collect();
            self.error(
                pointer,
                format!("data bindings {names:?} have a query record dependency cycle; their current records cannot initialize"),
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This match validates each closed query variant while keeping shared object and bounds checks together."
    )]
    fn check_data_query(
        &mut self,
        binding: &WidgetDataBinding,
        object: &EffectiveObjectType,
        index: usize,
        path: &str,
    ) {
        match &binding.query {
            WidgetDataQuery::ObjectList {
                filters,
                sort,
                page,
            } => {
                self.check_page_bounds(page, &format!("{path}/query/page"));
                for (i, filter) in filters.iter().enumerate() {
                    let filter_path = format!("{path}/query/filters/{i}");
                    self.check_filter(filter, &binding.binding_id, &filter_path);
                    self.check_filter_column(filter, &binding.binding_id, object, &filter_path);
                }
                if sort.len() > 1 {
                    self.error(
                        &format!("{path}/query/sort"),
                        "binding list sorting supports at most one sort key because indexes provide one exact property order",
                    );
                }
                for (i, key) in sort.iter().enumerate() {
                    self.check_sort(
                        key,
                        &binding.binding_id,
                        object,
                        &format!("{path}/query/sort/{i}"),
                    );
                }
            }
            WidgetDataQuery::ObjectRuntimeDetail {
                page_id,
                parameter_id,
            } => {
                self.check_context_page_at(page_id, &format!("{path}/query/pageId"));
                if !self
                    .context_parameter(page_id, parameter_id)
                    .is_some_and(|param| {
                        param.required
                            && param.value_type == norn_ir::WidgetValueType::Text
                            && param.runtime_key_of.as_ref() == Some(&binding.object)
                    })
                {
                    self.error(&format!("{path}/query/parameterId"),
                        "runtime detail requires a required text parameter marked with the exact bound entity");
                }
            }
            WidgetDataQuery::ObjectDetail { key } => {
                let supplied: Vec<&str> = key
                    .iter()
                    .map(|member| member.property_id.as_str())
                    .collect();
                if supplied.iter().collect::<BTreeSet<_>>().len() != supplied.len() {
                    self.error(
                        &format!("{path}/query/key"),
                        "object detail identity key contains a duplicate property",
                    );
                    return;
                }
                let matches = object.keys.iter().any(|identity| {
                    identity
                        .components
                        .iter()
                        .map(norn_ir::PropertyId::as_str)
                        .eq(supplied.iter().copied())
                });
                if !matches {
                    self.error(&format!("{path}/query/key"), "object detail key must exactly match one declared identity key in component order");
                }
                for (i, member) in key.iter().enumerate() {
                    let Some(property) = object.property(&member.property_id) else {
                        self.error(
                            &format!("{path}/query/key/{i}/propertyId"),
                            "identity property does not exist on the bound object",
                        );
                        continue;
                    };
                    self.check_filter_value(
                        &member.value,
                        property,
                        &format!("{path}/query/key/{i}/value"),
                        None,
                    );
                }
            }
            WidgetDataQuery::LinkTraversal {
                source_binding_id,
                link_id,
                max_depth,
                max_rows,
                page,
            } => {
                self.check_page_bounds(page, &format!("{path}/query/page"));
                if max_depth.0 == 0 || max_rows.0 == 0 || page.max_size > *max_rows {
                    self.error(&format!("{path}/query"), "link traversal requires positive depth and row bounds, with page.maxSize no greater than maxRows");
                }
                let source_index = self
                    .ux
                    .data_bindings
                    .iter()
                    .position(|candidate| candidate.binding_id == *source_binding_id);
                let Some(source_index) = source_index else {
                    self.error(
                        &format!("{path}/query/sourceBindingId"),
                        "link traversal source binding does not exist",
                    );
                    return;
                };
                if source_index >= index {
                    self.error(
                        &format!("{path}/query/sourceBindingId"),
                        "link traversal source must be declared before its target binding",
                    );
                }
                let Some(source_binding) = self.ux.data_bindings.get(source_index).cloned() else {
                    self.error(
                        &format!("{path}/query/sourceBindingId"),
                        "link traversal source binding does not exist",
                    );
                    return;
                };
                if !self.record_source_available(source_binding_id, path) {
                    self.error(
                        &format!("{path}/query/sourceBindingId"),
                        "link traversal source must be a detail record or a single-selection list",
                    );
                }
                let Some(source_entity) = self.resolve_ref(
                    &source_binding.object.id,
                    &source_binding.object.version,
                    &ArtifactKind::Entity,
                    &format!("/payload/ux/dataBindings/{source_index}/object"),
                ) else {
                    return;
                };
                let Some(Ok(source_object)) = self.catalog.object_type(&source_entity.artifact_id)
                else {
                    self.error(
                        &format!("{path}/query/linkId"),
                        "source object type is unavailable",
                    );
                    return;
                };
                let source_object = source_object.clone();
                let link = self
                    .catalog
                    .links_for_object_type(source_object.name.as_str())
                    .and_then(|links| links.iter().find(|link| link.id.as_str() == link_id))
                    .cloned();
                let Some(link) = link else {
                    self.error(
                        &format!("{path}/query/linkId"),
                        "link is not declared on the source object type",
                    );
                    return;
                };
                let target_name =
                    self.entity_object_name(&binding.object, &format!("{path}/object"));
                if let Some(target_name) = target_name {
                    let valid_targets = self.catalog.target_types(link.target.as_str());
                    if target_name != link.target.as_str()
                        && !valid_targets.is_some_and(|targets| targets.contains(&target_name))
                    {
                        self.error(
                            &format!("{path}/object"),
                            "traversal result binding does not match the declared link target",
                        );
                    }
                }
                if link.cardinality.to == norn_ir::Multiplicity::One
                    && matches!(
                        &source_binding.query,
                        WidgetDataQuery::ObjectDetail { .. }
                            | WidgetDataQuery::ObjectRuntimeDetail { .. }
                    )
                    && max_rows.0 > 1
                {
                    self.error(
                        &format!("{path}/query/maxRows"),
                        "a single-valued link cannot return more than one row per source object",
                    );
                }
                if max_depth.0 > 1
                    && (source_object.name != object.name
                        || link.target.as_str() != source_object.name.as_str())
                {
                    self.error(
                        &format!("{path}/query/maxDepth"),
                        "traversal depth greater than one requires a recursive self-target link",
                    );
                }
            }
        }
    }

    fn check_page_bounds(&mut self, bounds: &norn_ir::WidgetPageBounds, path: &str) {
        if bounds.size.0 == 0 || bounds.max_size.0 == 0 || bounds.size > bounds.max_size {
            self.error(
                path,
                "page bounds require positive size and maxSize, with size no greater than maxSize",
            );
        }
    }

    fn check_filter(&mut self, filter: &WidgetFilter, binding_id: &str, path: &str) {
        if filter.property.binding_id != binding_id {
            self.error(
                &format!("{path}/property/bindingId"),
                "filter property must belong to the queried binding",
            );
        }
        let Some(property) = self.resolve_property(&filter.property, &format!("{path}/property"))
        else {
            return;
        };
        if is_ordered_filter_operator(filter.operator)
            && property_type(property).is_none_or(|ty| !is_orderable_type(&ty))
        {
            self.error(
                &format!("{path}/operator"),
                "ordered comparison requires a text, numeric, date, or timestamp property",
            );
        }
        if filter.operator == WidgetFilterOperator::In {
            match &filter.value {
                WidgetFilterValue::Literal {
                    value: JsonValue::Array(values),
                } => {
                    for (index, value) in values.iter().enumerate() {
                        self.check_literal_value(value, property, &format!("{path}/value/{index}"));
                    }
                }
                WidgetFilterValue::Record { property } if property.property_id == "$key" => {
                    self.error(
                        &format!("{path}/value"),
                        "runtime keys cannot supply query or filter values",
                    );
                }
                WidgetFilterValue::Literal { .. }
                | WidgetFilterValue::ContextParameter { .. }
                | WidgetFilterValue::Record { .. } => self.error(
                    &format!("{path}/value"),
                    "the in operator requires a literal array of values",
                ),
            }
        } else {
            self.check_filter_value(
                &filter.value,
                property,
                &format!("{path}/value"),
                Some(filter.operator),
            );
        }
    }

    fn check_filter_column(
        &mut self,
        filter: &WidgetFilter,
        binding_id: &str,
        object: &EffectiveObjectType,
        path: &str,
    ) {
        if filter.property.binding_id != binding_id {
            return;
        }
        let property_path = format!("{path}/property");
        if object.property(&filter.property.property_id).is_none() {
            return;
        }
        match self.binding_property_column_owner(object, &filter.property.property_id) {
            Ok(_) => {}
            Err(BindingColumnError::Computed) => self.error(
                &property_path,
                "computed filter properties have no stored column for a binding index",
            ),
            Err(BindingColumnError::Overflow) => self.error(
                &property_path,
                "filter property is stored in the overflow payload, not a physical column for a binding index",
            ),
            Err(BindingColumnError::NotDeclared) => self.error(
                &property_path,
                "filter property is not declared on the object hierarchy and has no physical column for a binding index",
            ),
            Err(BindingColumnError::Unavailable) => self.error(
                &property_path,
                "filter property storage cannot be resolved for a binding index",
            ),
        }
        // The type's existing filter semantics are checked above. Physical
        // column validation does not change how the value is compared.
    }

    fn check_sort(
        &mut self,
        sort: &WidgetSort,
        binding_id: &str,
        object: &EffectiveObjectType,
        path: &str,
    ) {
        if sort.property.binding_id != binding_id {
            self.error(
                &format!("{path}/property/bindingId"),
                "sort property must belong to the queried binding",
            );
            return;
        }
        let property_path = format!("{path}/property");
        let Some(property) = self.resolve_property(&sort.property, &property_path) else {
            return;
        };
        if property.computed {
            self.error(
                &property_path,
                "computed sort properties have no stored column and cannot provide exact indexed order",
            );
            return;
        }
        if !is_binding_index_sort_type(property) {
            self.error(
                &property_path,
                "sort property type has no exact binding-index order; supported types are text, enum, integer, boolean, and date",
            );
            return;
        }
        match self.binding_property_column_owner(object, &sort.property.property_id) {
            Ok(owner) => {
                let Some(root) = object.lineage.last() else {
                    self.error(
                        &property_path,
                        "sort property storage has no hierarchy root for an exact binding index",
                    );
                    return;
                };
                if owner != root.as_str() {
                    self.error(
                        &property_path,
                        "sort property is declared below the hierarchy root and cannot provide exact order from the root binding index",
                    );
                }
            }
            Err(BindingColumnError::Computed) => self.error(
                &property_path,
                "computed sort properties have no stored column and cannot provide exact indexed order",
            ),
            Err(BindingColumnError::Overflow) => self.error(
                &property_path,
                "sort property is stored in the overflow payload, not a physical column, and cannot provide exact indexed order",
            ),
            Err(BindingColumnError::NotDeclared) => self.error(
                &property_path,
                "sort property is not declared on the object hierarchy and has no physical column for exact indexed order",
            ),
            Err(BindingColumnError::Unavailable) => self.error(
                &property_path,
                "sort property storage cannot be resolved for exact binding-index order",
            ),
        }
    }

    /// Find the table that physically stores a property and whether it is a
    /// real SQLite column. Inherited properties stay on their oldest
    /// declaration's table; computed and overflow properties are not columns.
    fn binding_property_column_owner(
        &self,
        object: &EffectiveObjectType,
        property_id: &str,
    ) -> Result<String, BindingColumnError> {
        let mut declaration = None;
        for (lineage_index, name) in object.lineage.iter().enumerate().rev() {
            let Some(definition) = self.object_type_definition(name.as_str()) else {
                return Err(BindingColumnError::Unavailable);
            };
            if let Some(property) = definition
                .properties
                .iter()
                .find(|property| property.id.as_str() == property_id)
            {
                let property = property.clone();
                declaration = Some((
                    lineage_index,
                    name.as_str().to_owned(),
                    definition,
                    property,
                ));
                break;
            }
        }
        let Some((lineage_index, owner, definition, property)) = declaration else {
            return Err(BindingColumnError::NotDeclared);
        };
        if property.computed.is_some() {
            return Err(BindingColumnError::Computed);
        }

        let mut inherited = BTreeSet::new();
        for ancestor in object.lineage.iter().skip(lineage_index.saturating_add(1)) {
            let Some(ancestor) = self.object_type_definition(ancestor.as_str()) else {
                return Err(BindingColumnError::Unavailable);
            };
            inherited.extend(
                ancestor
                    .properties
                    .iter()
                    .map(|property| property.id.as_str().to_owned()),
            );
        }
        let stored: Vec<_> = definition
            .properties
            .iter()
            .filter(|candidate| {
                candidate.computed.is_none() && !inherited.contains(candidate.id.as_str())
            })
            .collect();
        let Some(column_index) = stored
            .iter()
            .position(|candidate| candidate.id.as_str() == property_id)
        else {
            return Err(BindingColumnError::NotDeclared);
        };

        // The projection reserves five system columns on a hierarchy root
        // and three on a subtype table. When properties exceed that budget,
        // one property slot becomes `_overflow`; only the earlier properties
        // remain independently indexable columns.
        let system_columns = if definition.extends.is_none() { 5 } else { 3 };
        let property_budget = MAX_SQLITE_COLUMNS_PER_TABLE.saturating_sub(system_columns);
        let physical_columns = if stored.len() > property_budget {
            property_budget.saturating_sub(1)
        } else {
            stored.len()
        };
        if column_index >= physical_columns {
            return Err(BindingColumnError::Overflow);
        }
        Ok(owner)
    }

    fn object_type_definition(&self, name: &str) -> Option<norn_ir::ObjectTypeDef> {
        self.all.iter().find_map(|artifact| {
            let ArtifactPayload::Entity(payload) = &artifact.payload else {
                return None;
            };
            let Ok(Some(entity)) = entity_object_type(payload) else {
                return None;
            };
            if entity.root.object_type.name.as_str() == name {
                Some(entity.root.object_type)
            } else {
                None
            }
        })
    }

    fn check_filter_value(
        &mut self,
        value: &WidgetFilterValue,
        property: &EffectiveProperty,
        path: &str,
        operator: Option<WidgetFilterOperator>,
    ) {
        match value {
            WidgetFilterValue::Literal { value } => {
                if matches!(value, JsonValue::Null)
                    && operator.is_some_and(is_ordered_filter_operator)
                {
                    self.error(path, "ordered filter values cannot be null");
                } else {
                    self.check_literal_value(value, property, path);
                }
            }
            WidgetFilterValue::ContextParameter {
                page_id,
                parameter_id,
            } => {
                self.check_context_page_at(page_id, path);
                let Some(page) = self.pages.get(page_id.as_str()) else {
                    self.error(path, "context value names an unknown page");
                    return;
                };
                let Some(param) = page
                    .context_params
                    .iter()
                    .find(|param| param.param_id == *parameter_id)
                else {
                    self.error(path, "context value names an unknown page parameter");
                    return;
                };
                let actual = value_type(param.value_type, param.required);
                let Some(expected) = property_type(property) else {
                    self.error(path, "filter property has an unresolved or opaque type");
                    return;
                };
                if !types_compatible(&actual, &expected) {
                    self.error(
                        path,
                        "context parameter type is incompatible with the filter property",
                    );
                }
                if matches!(actual, Type::Optional(_)) {
                    self.error(
                        path,
                        "optional context parameters cannot supply a filter value",
                    );
                }
                if param.runtime_key_of.is_some() {
                    self.error(
                        path,
                        "runtime-key context parameters cannot supply query or filter values",
                    );
                }
                if operator.is_some_and(is_ordered_filter_operator)
                    && matches!(actual, Type::Bool | Type::Json)
                {
                    self.error(
                        path,
                        "ordered filter value must be text, numeric, date, or timestamp",
                    );
                }
            }
            WidgetFilterValue::Record { property } if property.property_id == "$key" => {
                self.error(path, "runtime keys cannot supply query or filter values");
            }
            WidgetFilterValue::Record { property: source } => {
                if !self.bindings.contains_key(source.binding_id.as_str()) {
                    self.error(path, "record value names an unknown data binding");
                    return;
                }
                if !self.record_source_available(&source.binding_id, path) {
                    self.error(path, "record value requires a detail record, list row, or single selection context");
                }
                let Some(source_property) = self.resolve_property(source, path) else {
                    return;
                };
                let (Some(actual), Some(expected)) =
                    (property_type(source_property), property_type(property))
                else {
                    self.error(
                        path,
                        "record value has an unresolved or opaque property type",
                    );
                    return;
                };
                if !types_compatible(&actual, &expected) {
                    self.error(
                        path,
                        "record value property type is incompatible with the target property",
                    );
                }
                if operator.is_some_and(is_ordered_filter_operator) && !is_orderable_type(&actual) {
                    self.error(
                        path,
                        "ordered filter value must be text, numeric, date, or timestamp",
                    );
                }
            }
        }
    }

    fn list_rendered_at(&self, binding_id: &str, path: &str) -> bool {
        let pages = self.pages_at(path);
        !pages.is_empty()
            && self.rendered_lists.get(binding_id).is_some_and(|rendered| {
                pages
                    .iter()
                    .all(|page| page.is_some() && rendered.contains(page))
            })
    }

    fn selection_available_at(&self, binding_id: &str, path: &str) -> bool {
        if !self.list_rendered_at(binding_id, path) {
            return false;
        }
        let placement_matches = |placement: &WidgetActionPlacement| {
            placement.location == WidgetActionLocation::Selection
                && placement.selection_binding_id.as_deref() == Some(binding_id)
        };
        if let Some(index) = path_index(path, "/payload/ux/actionBindings/") {
            let Some(binding) = self.ux.action_bindings.get(index) else {
                return false;
            };
            if self
                .ux
                .forms
                .iter()
                .any(|form| form.submit_binding_id == binding.binding_id)
            {
                return false;
            }
            let mut placements = self
                .ux
                .action_placements
                .iter()
                .filter(|placement| placement.binding_id == binding.binding_id);
            return placements.clone().next().is_some()
                && placements
                    .all(|placement| placement.form_id.is_none() && placement_matches(placement));
        }
        if let Some(index) = path_index(path, "/payload/ux/actionPlacements/") {
            return self
                .ux
                .action_placements
                .get(index)
                .is_some_and(placement_matches);
        }
        if let Some(index) = path_index(path, "/payload/ux/fields/") {
            return self
                .ux
                .fields
                .get(index)
                .is_some_and(|field| matches!(&field.target, WidgetFieldTarget::Property { .. }));
        }
        if path_index(path, "/payload/ux/overrides/").is_some() {
            return self
                .usage_at(path)
                .filter(|usage| {
                    !matches!(usage, Usage::Semantic(id) if self.form_owned_semantics.contains(id))
                })
                .and_then(|usage| self.usage_surfaces.get(&usage))
                .is_some_and(|surfaces| {
                    !surfaces.is_empty()
                        && surfaces
                            .iter()
                            .all(|surface| !matches!(surface, ActionSurface::Form(_)))
                });
        }
        path_index(path, "/payload/ux/navigationItems/").is_some()
    }

    fn record_source_available(&self, binding_id: &str, path: &str) -> bool {
        self.bindings.get(binding_id).is_some_and(|binding| {
            matches!(
                &binding.query,
                WidgetDataQuery::ObjectDetail { .. } | WidgetDataQuery::ObjectRuntimeDetail { .. }
            )
        }) || (self.list_selections.get(binding_id) == Some(&WidgetSelectionMode::Single)
            && self.list_rendered_at(binding_id, path))
    }

    fn value_source_available_at(
        &self,
        binding_id: &str,
        path: &str,
        form: Option<&WidgetForm>,
    ) -> bool {
        match form {
            Some(form) => {
                form.data_binding_id.as_deref() == Some(binding_id)
                    && self.record_source_available_for_form(form, binding_id)
            }
            None => self.record_source_available_at(binding_id, path),
        }
    }

    fn record_source_available_for_form(&self, form: &WidgetForm, binding_id: &str) -> bool {
        let Some(index) = self
            .ux
            .forms
            .iter()
            .position(|candidate| candidate.form_id == form.form_id)
        else {
            return false;
        };
        let path = format!("/payload/ux/forms/{index}");
        self.record_source_available(binding_id, &path)
            || (self.list_row_contexts.contains(binding_id)
                && self.list_rendered_at(binding_id, &path)
                && self.ux.action_placements.iter().any(|placement| {
                    placement.form_id.as_deref() == Some(form.form_id.as_str())
                        && placement.selection_binding_id.as_deref() == Some(binding_id)
                        && match placement.location {
                            WidgetActionLocation::Row => true,
                            WidgetActionLocation::Selection => {
                                self.list_selections.get(binding_id)
                                    == Some(&WidgetSelectionMode::Single)
                            }
                            WidgetActionLocation::Toolbar
                            | WidgetActionLocation::Detail
                            | WidgetActionLocation::FormFooter => false,
                        }
                }))
    }

    fn placement_draft_source_available(
        &self,
        placement: &WidgetActionPlacement,
        form_id: &str,
    ) -> bool {
        if let Some(opened_form) = &placement.form_id {
            return opened_form == form_id
                && placement.location != WidgetActionLocation::FormFooter;
        }
        placement.location == WidgetActionLocation::FormFooter
            && self
                .usage_surfaces
                .get(&Usage::Placement(placement.placement_id.clone()))
                .is_some_and(|surfaces| {
                    !surfaces.is_empty()
                        && surfaces.iter().all(
                            |surface| matches!(surface, ActionSurface::Form(id) if id == form_id),
                        )
                })
    }

    fn action_draft_source_available(
        &self,
        binding: &WidgetUxActionBinding,
        form_id: &str,
    ) -> bool {
        let mut placements = self
            .ux
            .action_placements
            .iter()
            .filter(|placement| placement.binding_id == binding.binding_id);
        let mut forms = self
            .ux
            .forms
            .iter()
            .filter(|form| form.submit_binding_id == binding.binding_id);
        let has_uses = placements.clone().next().is_some() || forms.clone().next().is_some();
        has_uses
            && placements.all(|placement| self.placement_draft_source_available(placement, form_id))
            && forms.all(|form| form.form_id == form_id)
    }

    fn draft_source_available_at(&self, form_id: &str, path: &str) -> bool {
        let Some(usage) = self.usage_at(path) else {
            return false;
        };
        if let Usage::Action(id) = &usage {
            return self
                .action_bindings
                .get(id.as_str())
                .is_some_and(|binding| self.action_draft_source_available(binding, form_id));
        }
        self.usage_surfaces.get(&usage).is_some_and(|surfaces| {
            !surfaces.is_empty()
                && surfaces
                    .iter()
                    .all(|surface| matches!(surface, ActionSurface::Form(id) if id == form_id))
        })
    }

    fn action_record_source_available(
        &self,
        action_binding_index: usize,
        record_binding_id: &str,
    ) -> bool {
        let Some(binding) = self.ux.action_bindings.get(action_binding_index) else {
            return false;
        };
        if binding.inputs.record_binding_id.as_deref() != Some(record_binding_id) {
            return false;
        }
        let path = format!("/payload/ux/actionBindings/{action_binding_index}");
        if self.is_bounded_list_binding(record_binding_id)
            && !self.list_rendered_at(record_binding_id, &path)
        {
            return false;
        }
        let mut placements = self
            .ux
            .action_placements
            .iter()
            .filter(|placement| placement.binding_id == binding.binding_id);
        let mut forms = self
            .ux
            .forms
            .iter()
            .filter(|form| form.submit_binding_id == binding.binding_id);
        let has_uses = self
            .ux
            .action_placements
            .iter()
            .any(|placement| placement.binding_id == binding.binding_id)
            || self
                .ux
                .forms
                .iter()
                .any(|form| form.submit_binding_id == binding.binding_id);
        let placements_are_compatible = placements.all(|placement| match placement.location {
            WidgetActionLocation::Row => {
                placement.selection_binding_id.as_deref() == Some(record_binding_id)
            }
            WidgetActionLocation::Selection => {
                placement.selection_binding_id.as_deref() == Some(record_binding_id)
                    && self.list_selections.get(record_binding_id)
                        == Some(&WidgetSelectionMode::Single)
            }
            WidgetActionLocation::Detail => self.record_source_available(record_binding_id, &path),
            WidgetActionLocation::FormFooter => binding
                .inputs
                .draft_form_id
                .as_deref()
                .and_then(|form_id| self.forms.get(form_id).copied())
                .is_some_and(|form| {
                    form.data_binding_id.as_deref() == Some(record_binding_id)
                        && self.record_source_available_for_form(form, record_binding_id)
                }),
            WidgetActionLocation::Toolbar => false,
        });
        let forms_are_compatible = forms.all(|form| {
            form.data_binding_id.as_deref() == Some(record_binding_id)
                && self.record_source_available_for_form(form, record_binding_id)
        });
        has_uses && placements_are_compatible && forms_are_compatible
    }

    fn record_source_available_at(&self, binding_id: &str, path: &str) -> bool {
        if self.is_bounded_list_binding(binding_id) && !self.list_rendered_at(binding_id, path) {
            return false;
        }
        if let Some(index) = path_index(path, "/payload/ux/navigationItems/") {
            let Some(item) = self.ux.navigation_items.get(index) else {
                return false;
            };
            return self
                .navigation_record_contexts(&item.item_id, path)
                .contains(binding_id);
        }
        if let Some(index) = path_index(path, "/payload/ux/actionBindings/") {
            return self.action_record_source_available(index, binding_id);
        }
        if let Some(index) = path_index(path, "/payload/ux/forms/") {
            return self.ux.forms.get(index).is_some_and(|form| {
                form.data_binding_id.as_deref() == Some(binding_id)
                    && self.record_source_available_for_form(form, binding_id)
            });
        }
        if self.record_source_available(binding_id, path) {
            return true;
        }
        if !self.list_row_contexts.contains(binding_id) {
            return false;
        }
        if let Some(index) = path_index(path, "/payload/ux/fields/") {
            return self
                .ux
                .fields
                .get(index)
                .is_some_and(|field| match &field.target {
                    WidgetFieldTarget::Property { property } => {
                        property.binding_id == binding_id
                            && self.row_record_available_at(binding_id, path)
                    }
                    WidgetFieldTarget::ChildRowProperty {
                        form_id,
                        collection_id,
                        ..
                    } => child_grid_component(self.ux, form_id, collection_id).is_some_and(
                        |component| {
                            matches!(
                                component,
                                WidgetComponent::ChildGrid {
                                    data_binding_id: Some(grid_binding),
                                    ..
                                } if grid_binding == binding_id
                            ) && self.row_record_available_at(binding_id, path)
                        },
                    ),
                    WidgetFieldTarget::FormInput { form_id, .. }
                    | WidgetFieldTarget::ParentProperty { form_id, .. } => self
                        .forms
                        .get(form_id.as_str())
                        .copied()
                        .is_some_and(|form| {
                            self.record_source_available_for_form(form, binding_id)
                        }),
                });
        }
        if let Some(index) = path_index(path, "/payload/ux/actionPlacements/") {
            return self
                .ux
                .action_placements
                .get(index)
                .is_some_and(|placement| {
                    placement.location == WidgetActionLocation::Row
                        && placement.selection_binding_id.as_deref() == Some(binding_id)
                });
        }
        path_index(path, "/payload/ux/overrides/").is_some()
            && self.row_record_available_at(binding_id, path)
    }

    fn row_record_available_at(&self, binding_id: &str, path: &str) -> bool {
        self.usage_at(path)
            .and_then(|usage| self.usage_surfaces.get(&usage))
            .is_some_and(|surfaces| {
                !surfaces.is_empty()
                    && surfaces.iter().all(
                        |surface| matches!(surface, ActionSurface::List(id) if id == binding_id),
                    )
            })
    }

    fn check_literal_value(&mut self, value: &JsonValue, property: &EffectiveProperty, path: &str) {
        if matches!(value, JsonValue::Null) && property.nullable {
            return;
        }
        let Ok(value) = crate::entity_instance::instance_json(value) else {
            self.error(path, "literal cannot be represented as a runtime value");
            return;
        };
        if let Err(error) = property.check_value(&value) {
            self.error(
                path,
                format!("literal does not match its property: {error:?}"),
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This pass connects each UX field presentation to its typed source and rendered tree."
    )]
    fn check_tree_and_fields(&mut self) {
        let fields = self.ux.fields.clone();
        for (index, field) in fields.iter().enumerate() {
            let path = format!("/payload/ux/fields/{index}");
            if field.label.0.is_empty()
                || field.label.0.values().any(|value| value.trim().is_empty())
            {
                self.error(
                    &format!("{path}/label"),
                    "field label must contain nonblank localized text",
                );
            }
            if let Some(help) = &field.help
                && help.0.values().any(|value| value.trim().is_empty())
            {
                self.error(
                    &format!("{path}/help"),
                    "help text values must not be blank",
                );
            }
            if field.format == Some(WidgetFieldFormat::MetricBuckets)
                && !matches!(&field.target, WidgetFieldTarget::Property { .. })
            {
                self.error(
                    &format!("{path}/format"),
                    "metric_buckets requires a display presentation bound to a json-ext property, not an input",
                );
            }
            match &field.target {
                WidgetFieldTarget::Property { property } => {
                    if field.format == Some(WidgetFieldFormat::MetricBuckets)
                        && let Some(resolved) =
                            self.resolve_property(property, &format!("{path}/target/property"))
                        && !matches!(
                            &resolved.kind,
                            EffectiveKind::Base(PropertyType::JsonExt { .. })
                        )
                    {
                        self.error(
                            &format!("{path}/format"),
                            "metric_buckets requires a display presentation bound to a json-ext property",
                        );
                    }
                    if let Some(resolved_property) =
                        self.resolve_property(property, &format!("{path}/target/property"))
                        && let Some(type_) = property_type(resolved_property)
                    {
                        if !self.value_source_available_at(&property.binding_id, &path, None)
                            && self
                                .usage_surfaces
                                .contains_key(&Usage::Field(field.field_id.clone()))
                        {
                            self.error(
                                &path,
                                "field value requires an available detail, row, or single-selection record at every rendering site",
                            );
                        }
                        self.check_control_type(
                            field.control,
                            field.format,
                            field.option_query_binding_id.as_deref(),
                            &type_,
                            &path,
                        );
                    }
                    if let Some(binding_id) = &field.option_query_binding_id
                        && !self.is_bounded_list_binding(binding_id)
                    {
                        self.error(
                            &format!("{path}/optionQueryBindingId"),
                            "option query must be a bounded list or traversal binding",
                        );
                    }
                }
                WidgetFieldTarget::FormInput {
                    form_id,
                    input_name,
                    initial,
                } => {
                    let Some(form) = self.forms.get(form_id.as_str()).copied() else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "form input target names an unknown form",
                        );
                        continue;
                    };
                    if !form.input_names.contains(input_name) {
                        self.error(
                            &format!("{path}/target/inputName"),
                            "form input target is not included by its form",
                        );
                    }
                    let Some(action_binding) = self
                        .action_bindings
                        .get(form.submit_binding_id.as_str())
                        .copied()
                    else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "form input submit binding does not exist",
                        );
                        continue;
                    };
                    let Some((schema, required)) = self
                        .actions
                        .get(action_binding.binding_id.as_str())
                        .and_then(|action| action.fields.get(input_name))
                        .cloned()
                    else {
                        self.error(
                            &format!("{path}/target/inputName"),
                            "form input does not resolve in the submit ActionIR schema",
                        );
                        continue;
                    };
                    if self.action_for_form(form).is_some_and(|action| {
                        matches!(
                            action.root.effect_program.as_ref(),
                            Some(ActionEffectProgram::MutateParentWithRows {
                                parent_values_field,
                                row_ops_field,
                                ..
                            }) if input_name == parent_values_field || input_name == row_ops_field
                        )
                    }) {
                        self.error(&format!("{path}/target/inputName"), "parent values and row operations are composed by nested fields and ChildGrid, not scalar form inputs");
                    }
                    let mut initialization = FormInputInitialization {
                        required,
                        source_available: false,
                    };
                    let input_type = self.schema_control_type(&schema);
                    self.check_control_type(
                        field.control,
                        field.format,
                        field.option_query_binding_id.as_deref(),
                        &input_type,
                        &path,
                    );
                    if let Some(binding_id) = &field.option_query_binding_id
                        && !self.is_bounded_list_binding(binding_id)
                    {
                        self.error(
                            &format!("{path}/optionQueryBindingId"),
                            "option query must be a bounded list or traversal binding",
                        );
                    }
                    if let Some(initial) = initial {
                        let mut source_available = true;
                        let binding_matches_form =
                            form.data_binding_id.as_deref() == Some(initial.binding_id.as_str());
                        if !binding_matches_form {
                            source_available = false;
                            if form.data_binding_id.is_none() {
                                self.error(
                                    &format!("{path}/target/initial"),
                                    "form input prefill requires a form record binding",
                                );
                            } else {
                                self.error(&format!("{path}/target/initial/bindingId"), "prefill property must belong to the form's declared record binding");
                            }
                        }
                        if binding_matches_form
                            && !self.value_source_available_at(
                                &initial.binding_id,
                                &path,
                                Some(form),
                            )
                        {
                            source_available = false;
                            self.error(
                                &format!("{path}/target/initial"),
                                "prefill record value is unavailable from the form's record source",
                            );
                        }
                        if let Some(property) =
                            self.resolve_property(initial, &format!("{path}/target/initial"))
                        {
                            if let Some(source_type) = property_type(property) {
                                if !type_fits_schema(&source_type, &schema) {
                                    source_available = false;
                                    self.error(
                                        &format!("{path}/target/initial"),
                                        "prefill property type does not fit the ActionIR form input",
                                    );
                                }
                            } else {
                                source_available = false;
                            }
                        } else {
                            source_available = false;
                        }
                        initialization.source_available = source_available;
                    }
                    self.check_display_initialization(
                        field.control,
                        &initialization,
                        &format!("{path}/target/initial"),
                    );
                    self.form_input_initialization
                        .insert(field.field_id.clone(), initialization);
                    if !self
                        .presented_form_inputs
                        .insert((form_id.clone(), input_name.clone()))
                    {
                        self.error(
                            &format!("{path}/target"),
                            "a form input must have exactly one field presentation",
                        );
                    }
                }
                WidgetFieldTarget::ParentProperty {
                    form_id,
                    input_name,
                    property_id,
                } => {
                    let Some(form) = self.forms.get(form_id.as_str()).copied() else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "parent property target names an unknown form",
                        );
                        continue;
                    };
                    if !matches!(self.action_for_form(form), Some(action) if matches!(
                        action.root.effect_program.as_ref(),
                        Some(ActionEffectProgram::MutateParentWithRows { parent_values_field, .. })
                            if parent_values_field == input_name
                    )) {
                        self.error(&format!("{path}/target/inputName"), "parent property must belong to the parent-values object of a parent-and-rows action");
                    }
                    if !form.input_names.contains(input_name) {
                        self.error(
                            &format!("{path}/target/inputName"),
                            "parent-values field is not included by its form",
                        );
                    }
                    let Some(action) = self.action_for_form(form).cloned() else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "parent property form submit action is unavailable",
                        );
                        continue;
                    };
                    let Some(property) = self
                        .parent_object(&action)
                        .and_then(|object| object.property(property_id).cloned())
                    else {
                        self.error(
                            &format!("{path}/target/propertyId"),
                            "parent property does not exist on the action's pinned entity",
                        );
                        continue;
                    };
                    let parent_schema = action.fields.get(input_name).map(|(schema, _)| schema);
                    let Some(type_) = property_type(&property) else {
                        self.error(
                            &format!("{path}/target/propertyId"),
                            "parent property type is unresolved",
                        );
                        continue;
                    };
                    if parent_schema.is_none_or(|schema| !matches!(schema, Schema::Json)) {
                        self.error(&format!("{path}/target/propertyId"), "parent property must be written through the bounded JSON parent-values input");
                    }
                    let lifecycle_managed = self
                        .parent_object(&action)
                        .is_some_and(|object| object.lifecycle(property_id).is_some());
                    let generated = self.parent_object(&action).is_some_and(|object| {
                        object.keys.iter().any(|key| {
                            key.naming_series.is_some()
                                && key.components.iter().any(|id| id.as_str() == property_id)
                        })
                    });
                    if (property.computed || lifecycle_managed || generated)
                        && field.control != WidgetFieldControl::Display
                    {
                        self.error(
                            &format!("{path}/control"),
                            "computed or lifecycle-managed parent properties are read-only; generated parent properties are also read-only",
                        );
                    }
                    self.check_control_type(
                        field.control,
                        field.format,
                        field.option_query_binding_id.as_deref(),
                        &type_,
                        &path,
                    );
                    if !self
                        .presented_form_inputs
                        .insert((form_id.clone(), input_name.clone()))
                    {
                        // Multiple nested properties intentionally share one top-level form input.
                    }
                }
                WidgetFieldTarget::ChildRowProperty {
                    form_id,
                    collection_id,
                    property_id,
                } => {
                    let Some(form) = self.forms.get(form_id.as_str()).copied() else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "child-row property target names an unknown form",
                        );
                        continue;
                    };
                    let Some(action) = self.action_for_form(form).cloned() else {
                        self.error(
                            &format!("{path}/target/formId"),
                            "child-row property form submit action is unavailable",
                        );
                        continue;
                    };
                    if child_grid_component(self.ux, form_id, collection_id).is_none() {
                        self.error(
                            &format!("{path}/target/collectionId"),
                            "child-row property must be included by its matching child grid",
                        );
                        continue;
                    }
                    let Some(property) = self
                        .child_object(&action, collection_id)
                        .and_then(|object| object.property(property_id).cloned())
                    else {
                        self.error(
                            &format!("{path}/target/propertyId"),
                            "child-row property does not exist on the owned child entity",
                        );
                        continue;
                    };
                    let Some(type_) = property_type(&property) else {
                        self.error(
                            &format!("{path}/target/propertyId"),
                            "child-row property type is unresolved",
                        );
                        continue;
                    };
                    let Some(collection) = self.parent_object(&action).and_then(|object| {
                        object
                            .owned_collections
                            .iter()
                            .find(|candidate| candidate.id.as_str() == collection_id)
                            .cloned()
                    }) else {
                        self.error(
                            &format!("{path}/target/collectionId"),
                            "child-row property names an undeclared owned collection",
                        );
                        continue;
                    };
                    let reserved = property_id == collection.row_id_property.as_str()
                        || property_id == collection.ordering_key.as_str();
                    let lifecycle_managed = self
                        .child_object(&action, collection_id)
                        .is_some_and(|object| object.lifecycle(property_id).is_some());
                    let action_derived = matches!(
                        action.root.effect_program.as_ref(),
                        Some(ActionEffectProgram::MutateParentWithRows { collections, .. })
                            if collections.iter().any(|declared| {
                                declared.id.as_str() == collection_id
                                    && declared.derive.contains_key(&property.id)
                            })
                    );
                    if (reserved || property.computed || lifecycle_managed || action_derived)
                        && field.control != WidgetFieldControl::Display
                    {
                        self.error(&format!("{path}/control"), "row identity, ordering, computed, lifecycle-managed properties are read-only in a child grid; action-derived properties must also be display-only");
                    }
                    self.check_control_type(
                        field.control,
                        field.format,
                        field.option_query_binding_id.as_deref(),
                        &type_,
                        &path,
                    );
                }
            }
            self.check_optional_condition(
                field.visible_when.as_ref(),
                &format!("{path}/visibleWhen"),
            );
            self.check_optional_condition(
                field.required_when.as_ref(),
                &format!("{path}/requiredWhen"),
            );
            self.check_optional_condition(
                field.read_only_when.as_ref(),
                &format!("{path}/readOnlyWhen"),
            );
        }
        let pages = self.ux.pages.clone();
        for (page_index, page) in pages.iter().enumerate() {
            self.check_node(
                &page.root,
                &format!("/payload/ux/pages/{page_index}/root"),
                ActionSurface::Page,
            );
        }
        for (index, field) in fields.iter().enumerate() {
            let path = format!("/payload/ux/fields/{index}");
            match &field.target {
                WidgetFieldTarget::Property { property }
                    if !self
                        .included_fields
                        .contains(&(property.binding_id.clone(), property.property_id.clone())) =>
                {
                    self.error(&path, "property presentation is not explicitly included by a section, list, or field component");
                }
                WidgetFieldTarget::FormInput {
                    form_id,
                    input_name,
                    ..
                } if !self
                    .included_form_inputs
                    .contains(&(form_id.clone(), input_name.clone())) =>
                {
                    self.error(
                        &path,
                        "form input presentation is not included by its form component",
                    );
                }
                WidgetFieldTarget::ParentProperty {
                    form_id,
                    input_name,
                    ..
                } if !self.included_form_inputs.contains(&(form_id.clone(), input_name.clone())) => {
                    self.error(&path, "parent property presentation is not included by its matching form");
                }
                WidgetFieldTarget::ChildRowProperty {
                    form_id,
                    collection_id,
                    ..
                } if !child_grid_component(self.ux, form_id, collection_id).is_some_and(|component| {
                    matches!(component, WidgetComponent::ChildGrid { field_ids, .. } if field_ids.contains(&field.field_id))
                }) => {
                    self.error(&path, "child-row property presentation is not included by its matching ChildGrid.fieldIds");
                }
                WidgetFieldTarget::Property { .. } | WidgetFieldTarget::FormInput { .. } | WidgetFieldTarget::ParentProperty { .. } | WidgetFieldTarget::ChildRowProperty { .. } => {}
            }
            if let WidgetFieldTarget::Property { property } = &field.target {
                let presentations = fields.iter().filter(|candidate| {
                    matches!(&candidate.target, WidgetFieldTarget::Property { property: other }
                        if other.binding_id == property.binding_id && other.property_id == property.property_id)
                }).count();
                if presentations != 1 {
                    self.error(
                        &path,
                        "each included property must have exactly one field presentation",
                    );
                }
            }
            if let WidgetFieldTarget::ParentProperty {
                form_id,
                input_name,
                property_id,
            } = &field.target
            {
                let presentations = fields.iter().filter(|candidate| {
                    matches!(&candidate.target, WidgetFieldTarget::ParentProperty {
                        form_id: other_form,
                        input_name: other_input,
                        property_id: other_property,
                    } if other_form == form_id && other_input == input_name && other_property == property_id)
                }).count();
                if presentations != 1 {
                    self.error(
                        &path,
                        "each nested parent property must have exactly one field presentation",
                    );
                }
            }
            if let WidgetFieldTarget::ChildRowProperty {
                form_id,
                collection_id,
                property_id,
            } = &field.target
            {
                let presentations = fields.iter().filter(|candidate| {
                    matches!(&candidate.target, WidgetFieldTarget::ChildRowProperty {
                        form_id: other_form,
                        collection_id: other_collection,
                        property_id: other_property,
                    } if other_form == form_id && other_collection == collection_id && other_property == property_id)
                }).count();
                if presentations != 1 {
                    self.error(
                        &path,
                        "each child-row property must have exactly one field presentation",
                    );
                }
            }
        }
        for (index, form) in self.ux.forms.iter().enumerate() {
            for (name_index, name) in form.input_names.iter().enumerate() {
                if !self
                    .presented_form_inputs
                    .contains(&(form.form_id.clone(), name.clone()))
                    || !self
                        .included_form_inputs
                        .contains(&(form.form_id.clone(), name.clone()))
                {
                    self.error(
                        &format!("/payload/ux/forms/{index}/inputNames/{name_index}"),
                        "each included form input needs exactly one field presentation",
                    );
                }
            }
        }
        for (binding_id, property_id) in self.included_fields.clone() {
            let presentations = fields
                .iter()
                .filter(|field| {
                    matches!(&field.target, WidgetFieldTarget::Property { property }
                    if property.binding_id == binding_id && property.property_id == property_id)
                })
                .count();
            if presentations != 1 {
                self.error("/payload/ux/fields", format!("included property {binding_id}.{property_id} requires exactly one field presentation"));
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This recursive case walk validates one closed Widget UX component and its child surface."
    )]
    fn check_node(&mut self, node: &WidgetNode, path: &str, surface: ActionSurface) {
        match &node.component {
            WidgetComponent::Section { section } => {
                if section.label.0.is_empty()
                    || section
                        .label
                        .0
                        .values()
                        .any(|value| value.trim().is_empty())
                {
                    self.error(
                        &format!("{path}/component/section/label"),
                        "section label must contain nonblank localized text",
                    );
                }
                for (index, property) in section.fields.iter().enumerate() {
                    self.include_property(
                        property,
                        &format!("{path}/component/section/fields/{index}"),
                    );
                }
            }
            WidgetComponent::List {
                data_binding_id,
                fields,
                ..
            } => {
                if !self
                    .bindings
                    .get(data_binding_id.as_str())
                    .is_some_and(|binding| {
                        matches!(
                            &binding.query,
                            WidgetDataQuery::ObjectList { .. }
                                | WidgetDataQuery::LinkTraversal { .. }
                        )
                    })
                {
                    self.error(
                        &format!("{path}/component/dataBindingId"),
                        "list component requires an existing bounded list or traversal binding",
                    );
                }
                for (index, property) in fields.iter().enumerate() {
                    if property.binding_id != *data_binding_id {
                        self.error(
                            &format!("{path}/component/fields/{index}/bindingId"),
                            "list field must belong to the list binding",
                        );
                    }
                    self.include_property(property, &format!("{path}/component/fields/{index}"));
                }
            }
            WidgetComponent::Detail { data_binding_id } => {
                if !self
                    .bindings
                    .get(data_binding_id.as_str())
                    .is_some_and(|binding| {
                        matches!(
                            &binding.query,
                            WidgetDataQuery::ObjectDetail { .. }
                                | WidgetDataQuery::ObjectRuntimeDetail { .. }
                        )
                    })
                {
                    self.error(
                        &format!("{path}/component/dataBindingId"),
                        "detail component requires an object-detail binding",
                    );
                }
            }
            WidgetComponent::Form { form_id } => {
                if !self.forms.contains_key(form_id.as_str()) {
                    self.error(
                        &format!("{path}/component/formId"),
                        "form component names an unknown form",
                    );
                }
            }
            WidgetComponent::ChildGrid {
                form_id,
                label,
                collection_id,
                ordering_key,
                data_binding_id,
                row_ops_input_name,
                max_rows,
                field_ids,
            } => self.check_child_grid(
                form_id,
                label,
                collection_id,
                ordering_key,
                data_binding_id.as_deref(),
                row_ops_input_name,
                max_rows.0,
                field_ids,
                &surface,
                path,
            ),
            WidgetComponent::Field { field_id } => {
                let Some(field) = self
                    .ux
                    .fields
                    .iter()
                    .find(|field| field.field_id == *field_id)
                    .cloned()
                else {
                    self.error(
                        &format!("{path}/component/fieldId"),
                        "field component names an unknown presentation",
                    );
                    return;
                };
                if !self.used_field_ids.insert(field_id.clone()) {
                    self.error(
                        &format!("{path}/component/fieldId"),
                        "field presentation may appear only once in the component tree",
                    );
                }
                match &field.target {
                    WidgetFieldTarget::Property { property } => {
                        if matches!(&surface, ActionSurface::Form(_)) {
                            self.error(&format!("{path}/component/fieldId"), "form children must present form inputs, not arbitrary object properties");
                        }
                        self.included_fields
                            .insert((property.binding_id.clone(), property.property_id.clone()));
                        let type_ = self
                            .resolve_property(property, &format!("{path}/component/fieldId"))
                            .and_then(property_type);
                        let option_type = field.option_query_binding_id.as_deref().and_then(|id| {
                            self.option_binding_key_type(id, &format!("{path}/component/fieldId"))
                        });
                        self.register_semantic_node(
                            &field.field_id,
                            type_.clone(),
                            true,
                            false,
                            option_type.clone(),
                            &format!("{path}/component/fieldId"),
                        );
                        if let Some(semantic_id) = &node.semantic_id {
                            self.register_semantic_node(
                                semantic_id,
                                type_,
                                true,
                                false,
                                option_type,
                                &format!("{path}/semanticId"),
                            );
                        }
                    }
                    WidgetFieldTarget::FormInput {
                        form_id,
                        input_name,
                        ..
                    } => {
                        if !matches!(&surface, ActionSurface::Form(parent_form) if parent_form == form_id)
                        {
                            self.error(
                                &format!("{path}/component/fieldId"),
                                "form input field must be a child of its matching form component",
                            );
                        }
                        if !self
                            .included_form_inputs
                            .insert((form_id.clone(), input_name.clone()))
                        {
                            self.error(
                                &format!("{path}/component/fieldId"),
                                "form input may be presented only once in its form",
                            );
                        }
                        let schema = self
                            .forms
                            .get(form_id.as_str())
                            .and_then(|form| {
                                self.action_bindings.get(form.submit_binding_id.as_str())
                            })
                            .and_then(|binding| self.actions.get(binding.binding_id.as_str()))
                            .and_then(|action| action.fields.get(input_name))
                            .map(|(schema, _)| schema.clone());
                        if let Some(schema) = schema {
                            let type_ = self.schema_control_type(&schema);
                            let option_type =
                                field.option_query_binding_id.as_deref().and_then(|id| {
                                    self.option_binding_key_type(
                                        id,
                                        &format!("{path}/component/fieldId"),
                                    )
                                });
                            self.register_semantic_node(
                                &field.field_id,
                                Some(type_.clone()),
                                true,
                                false,
                                option_type.clone(),
                                &format!("{path}/component/fieldId"),
                            );
                            let initialization = self
                                .form_input_initialization
                                .get(&field.field_id)
                                .cloned()
                                .unwrap_or_default();
                            self.set_form_input_initialization(&field.field_id, &initialization);
                            if let Some(semantic_id) = &node.semantic_id {
                                self.register_semantic_node(
                                    semantic_id,
                                    Some(type_),
                                    true,
                                    false,
                                    option_type,
                                    &format!("{path}/semanticId"),
                                );
                                self.set_form_input_initialization(semantic_id, &initialization);
                            }
                        } else {
                            let option_type =
                                field.option_query_binding_id.as_deref().and_then(|id| {
                                    self.option_binding_key_type(
                                        id,
                                        &format!("{path}/component/fieldId"),
                                    )
                                });
                            self.register_semantic_node(
                                &field.field_id,
                                None,
                                true,
                                false,
                                option_type,
                                &format!("{path}/component/fieldId"),
                            );
                        }
                    }
                    WidgetFieldTarget::ParentProperty {
                        form_id,
                        input_name,
                        property_id,
                    } => {
                        if !matches!(&surface, ActionSurface::Form(parent_form) if parent_form == form_id)
                        {
                            self.error(&format!("{path}/component/fieldId"), "parent property field must be a child of its matching form component");
                        }
                        self.included_form_inputs
                            .insert((form_id.clone(), input_name.clone()));
                        let value_type = self
                            .forms
                            .get(form_id.as_str())
                            .and_then(|form| self.action_for_form(form))
                            .and_then(|action| self.parent_object(action))
                            .and_then(|object| object.property(property_id).cloned())
                            .and_then(|property| property_type(&property));
                        let option_type = field.option_query_binding_id.as_deref().and_then(|id| {
                            self.option_binding_key_type(id, &format!("{path}/component/fieldId"))
                        });
                        self.register_semantic_node(
                            &field.field_id,
                            value_type.clone(),
                            true,
                            false,
                            option_type.clone(),
                            &format!("{path}/component/fieldId"),
                        );
                        if let Some(semantic_id) = &node.semantic_id {
                            self.register_semantic_node(
                                semantic_id,
                                value_type,
                                true,
                                false,
                                option_type,
                                &format!("{path}/semanticId"),
                            );
                        }
                    }
                    WidgetFieldTarget::ChildRowProperty { .. } => {
                        self.error(
                            &format!("{path}/component/fieldId"),
                            "child-row property fields are rendered by ChildGrid.fieldIds, not standalone field nodes",
                        );
                    }
                }
            }
            WidgetComponent::Action { placement_id } => {
                if let Some(placement) = self.placements.get(placement_id.as_str()).copied() {
                    self.check_action_surface(
                        placement,
                        &surface,
                        &format!("{path}/component/placementId"),
                    );
                    self.referenced_placements.insert(placement_id.clone());
                } else {
                    self.error(
                        &format!("{path}/component/placementId"),
                        "action component names an unknown placement",
                    );
                }
            }
            WidgetComponent::Navigation { item_id } => {
                if !self
                    .ux
                    .navigation_items
                    .iter()
                    .any(|item| item.item_id == *item_id)
                {
                    self.error(
                        &format!("{path}/component/itemId"),
                        "navigation component names an unknown item",
                    );
                }
            }
            WidgetComponent::Composition { .. } | WidgetComponent::Dialog { .. } => {}
        }
        self.check_children(node, path);
        let child_surface = match &node.component {
            WidgetComponent::List {
                data_binding_id, ..
            } => ActionSurface::List(data_binding_id.clone()),
            WidgetComponent::Detail { data_binding_id } => {
                ActionSurface::Detail(data_binding_id.clone())
            }
            WidgetComponent::Form { form_id } | WidgetComponent::ChildGrid { form_id, .. } => {
                ActionSurface::Form(form_id.clone())
            }
            WidgetComponent::Section { .. }
            | WidgetComponent::Dialog { .. }
            | WidgetComponent::Field { .. }
            | WidgetComponent::Action { .. }
            | WidgetComponent::Navigation { .. }
            | WidgetComponent::Composition { .. } => surface,
        };
        for (index, child) in node.children.iter().enumerate() {
            self.check_node(
                child,
                &format!("{path}/children/{index}"),
                child_surface.clone(),
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one closed grid contract binds its form, owned link, bounded read, and row presentations"
    )]
    fn check_child_grid(
        &mut self,
        form_id: &str,
        label: &LocaleText,
        collection_id: &str,
        ordering_key: &str,
        data_binding_id: Option<&str>,
        row_ops_input_name: &str,
        max_rows: u64,
        field_ids: &[String],
        surface: &ActionSurface,
        path: &str,
    ) {
        if label.0.is_empty() || label.0.values().any(|value| value.trim().is_empty()) {
            self.error(
                &format!("{path}/component/label"),
                "child grid label must contain nonblank localized text",
            );
        }
        if !self
            .child_grid_collections
            .insert((form_id.to_owned(), collection_id.to_owned()))
        {
            self.error(
                &format!("{path}/component/collectionId"),
                "one form may render an owned collection in only one child grid",
            );
        }
        if !matches!(surface, ActionSurface::Form(parent_form) if parent_form == form_id) {
            self.error(
                &format!("{path}/component/formId"),
                "child grid must be a child of its matching form component",
            );
        }
        let Some(form) = self.forms.get(form_id).copied() else {
            self.error(
                &format!("{path}/component/formId"),
                "child grid names an unknown form",
            );
            return;
        };
        let Some(action) = self.action_for_form(form).cloned() else {
            self.error(
                &format!("{path}/component/formId"),
                "child grid form action is unavailable",
            );
            return;
        };
        let Some(ActionEffectProgram::MutateParentWithRows {
            entity_artifact_id,
            entity_artifact_version,
            row_ops_field,
            collections,
            ..
        }) = action.root.effect_program.as_ref()
        else {
            self.error(
                &format!("{path}/component"),
                "child grid requires a parent-and-rows submit action",
            );
            return;
        };
        if row_ops_input_name != row_ops_field
            || !form
                .input_names
                .iter()
                .any(|name| name == row_ops_input_name)
            || !action
                .fields
                .get(row_ops_input_name)
                .is_some_and(|(schema, _)| matches!(schema, Schema::Json))
        {
            self.error(
                &format!("{path}/component/rowOpsInputName"),
                "child grid must populate the included bounded JSON input declared by its submit action",
            );
        }
        self.included_form_inputs
            .insert((form_id.to_owned(), row_ops_input_name.to_owned()));
        self.presented_form_inputs
            .insert((form_id.to_owned(), row_ops_input_name.to_owned()));
        let Some(parent) = self.parent_object(&action) else {
            self.error(
                &format!("{path}/component/collectionId"),
                "parent effect entity is unavailable",
            );
            return;
        };
        let Some(collection) = parent
            .owned_collections
            .iter()
            .find(|collection| collection.id.as_str() == collection_id)
            .cloned()
        else {
            self.error(
                &format!("{path}/component/collectionId"),
                "child grid names no owned collection on its action entity",
            );
            return;
        };
        if ordering_key != collection.ordering_key.as_str() {
            self.error(
                &format!("{path}/component/orderingKey"),
                "child grid orderingKey must match the owned collection's declared ordering property",
            );
        }
        if !collections
            .iter()
            .any(|declared| declared.id.as_str() == collection_id)
        {
            self.error(
                &format!("{path}/component/collectionId"),
                "submit action has no trusted behavior declaration for this collection",
            );
        }
        if max_rows == 0 || max_rows > collection.max_rows.0 {
            self.error(
                &format!("{path}/component/maxRows"),
                "child grid maxRows must be positive and no greater than the entity collection bound",
            );
        }

        match (form.mode, data_binding_id) {
            (WidgetFormMode::Edit, Some(binding_id)) => {
                let parent_binding_id = form.data_binding_id.as_deref();
                let Some(binding) = self.bindings.get(binding_id).copied().cloned() else {
                    self.error(
                        &format!("{path}/component/dataBindingId"),
                        "child grid traversal binding does not exist",
                    );
                    return;
                };
                let exact_traversal = matches!(
                    &binding.query,
                    WidgetDataQuery::LinkTraversal {
                        source_binding_id,
                        link_id,
                        max_depth,
                        max_rows: traversal_rows,
                        page,
                    } if Some(source_binding_id.as_str()) == parent_binding_id
                        && link_id == collection_id
                        && max_depth.0 == 1
                        && traversal_rows.0 >= collection.max_rows.0
                        && page.max_size.0 >= collection.max_rows.0
                );
                if !exact_traversal {
                    self.error(
                        &format!("{path}/component/dataBindingId"),
                        "edit grid requires a complete, bounded depth-one traversal of its owned link from the matching parent form binding",
                    );
                }
                if self
                    .entity_object_name(
                        &binding.object,
                        &format!("{path}/component/dataBindingId/object"),
                    )
                    .as_deref()
                    != Some(collection.child_type.as_str())
                {
                    self.error(
                        &format!("{path}/component/dataBindingId/object"),
                        "child grid traversal must read the exact owned child object type",
                    );
                }
                if let Some(parent_binding_id) = parent_binding_id
                    && self
                        .bindings
                        .get(parent_binding_id)
                        .is_none_or(|parent_binding| {
                            parent_binding.object.id != *entity_artifact_id
                                || parent_binding.object.version != *entity_artifact_version
                        })
                {
                    self.error(
                        &format!("{path}/component/dataBindingId"),
                        "child traversal source must bind the action's exact parent entity artifact",
                    );
                }
            }
            (WidgetFormMode::Edit, None) => self.error(
                &format!("{path}/component/dataBindingId"),
                "edit grid requires an authoritative owned-row traversal snapshot",
            ),
            (WidgetFormMode::Create, Some(_)) => self.error(
                &format!("{path}/component/dataBindingId"),
                "create grid starts empty and must not read rows from an uncommitted parent",
            ),
            (WidgetFormMode::Create, None) => {}
            (WidgetFormMode::ActionInput, _) => self.error(
                &format!("{path}/component/formId"),
                "child grid cannot belong to an action_input form",
            ),
        }

        let Some(child) = self.child_object(&action, collection_id) else {
            self.error(
                &format!("{path}/component/collectionId"),
                "owned child object type is unavailable",
            );
            return;
        };
        for lifecycle in &child.lifecycles {
            let property_id = lifecycle.lifecycle.property.as_str();
            let Some(property) = child.property(property_id) else {
                continue;
            };
            if !has_initial_lifecycle_default(property, &lifecycle.lifecycle.initial) {
                self.error(
                    &format!("{path}/component/fieldIds"),
                    format!(
                        "child grid Add requires lifecycle property {property_id:?} to declare a default in one of its initial states"
                    ),
                );
            }
        }
        let row_id_property = child.property(collection.row_id_property.as_str());
        if row_id_property.is_none_or(|property| {
            !matches!(
                &property.kind,
                EffectiveKind::Base(PropertyType::Text {
                    format: None | Some(norn_ir::TextFormat::Uuid)
                })
            ) || property.constraints.iter().any(|constraint| {
                constraint.min_length.is_some_and(|min| min.0 > 36)
                    || constraint.max_length.is_some_and(|max| max.0 < 36)
                    || constraint.pattern.is_some()
            })
        }) {
            self.error(
                &format!("{path}/component/collectionId"),
                "child grid row ID must admit generated UUIDs: text or UUID format, length including 36, and no additional pattern",
            );
        }
        let ordering_property = child.property(collection.ordering_key.as_str());
        if ordering_property.is_none_or(|property| {
            property.nullable
                || property.computed
                || !property_type(property).is_some_and(|type_| matches!(type_, Type::Integer))
        }) {
            self.error(
                &format!("{path}/component/orderingKey"),
                "owned collection ordering property must be a required stored integer on the child object",
            );
        }
        let mut grid_fields = BTreeSet::new();
        let mut editable_child_properties = BTreeSet::new();
        let derived_properties = collections
            .iter()
            .find(|declared| declared.id.as_str() == collection_id)
            .map(|declared| &declared.derive);
        for (index, field_id) in field_ids.iter().enumerate() {
            let field_path = format!("{path}/component/fieldIds/{index}");
            if !grid_fields.insert(field_id.as_str()) {
                self.error(&field_path, "child grid field ids must be unique");
                continue;
            }
            let Some(field) = self
                .ux
                .fields
                .iter()
                .find(|field| field.field_id == *field_id)
                .cloned()
            else {
                self.error(
                    &field_path,
                    "child grid field id does not name a field presentation",
                );
                continue;
            };
            if !self.used_field_ids.insert(field_id.clone()) {
                self.error(
                    &field_path,
                    "field presentation may appear only once across the widget tree and grids",
                );
            }
            let WidgetFieldTarget::ChildRowProperty {
                form_id: target_form,
                collection_id: target_collection,
                property_id,
            } = &field.target
            else {
                self.error(
                    &field_path,
                    "child grid field must target a child-row property",
                );
                continue;
            };
            if target_form != form_id || target_collection != collection_id {
                self.error(
                    &field_path,
                    "child grid field target must name the same form and owned collection",
                );
                continue;
            }
            let Some(property) = child.property(property_id) else {
                self.error(
                    &field_path,
                    "child grid field property does not exist on the child object type",
                );
                continue;
            };
            let Some(value_type) = property_type(property) else {
                self.error(
                    &field_path,
                    "child grid field property has an unresolved type",
                );
                continue;
            };
            let reserved = property_id == collection.row_id_property.as_str()
                || property_id == collection.ordering_key.as_str();
            let lifecycle_managed = child.lifecycle(property_id.as_str()).is_some();
            let action_derived =
                derived_properties.is_some_and(|derived| derived.contains_key(&property.id));
            if (reserved || property.computed || lifecycle_managed || action_derived)
                && field.control != WidgetFieldControl::Display
            {
                self.error(&field_path, "row id, ordering key, computed, lifecycle-managed child properties may only be displayed read-only; action-derived properties must also be display-only");
            }
            if !reserved
                && !property.computed
                && !lifecycle_managed
                && !action_derived
                && field.control != WidgetFieldControl::Display
                && field.visible_when.is_none()
                && field.read_only_when.is_none()
            {
                editable_child_properties.insert(property_id.as_str().to_owned());
            }
            self.check_control_type(
                field.control,
                field.format,
                field.option_query_binding_id.as_deref(),
                &value_type,
                &field_path,
            );
            let option_type = field
                .option_query_binding_id
                .as_deref()
                .and_then(|id| self.option_binding_key_type(id, &field_path));
            self.register_semantic_node(
                field_id,
                Some(value_type),
                true,
                false,
                option_type,
                &field_path,
            );
        }
        for property in child.properties.iter().filter(|property| {
            !property.nullable
                && !property.computed
                && property.default.is_none()
                && child.lifecycle(property.id.as_str()).is_none()
                && property.id != collection.row_id_property
                && property.id != collection.ordering_key
                && !derived_properties.is_some_and(|derived| derived.contains_key(&property.id))
        }) {
            if !editable_child_properties.contains(property.id.as_str()) {
                self.error(
                    &format!("{path}/component/fieldIds"),
                    format!(
                        "child grid must expose required child property {:?} as an unconditional editable field, or provide a declared default or derivation, so Add can create a complete row",
                        property.id.as_str()
                    ),
                );
            }
        }
    }

    fn check_action_surface(
        &self,
        placement: &WidgetActionPlacement,
        surface: &ActionSurface,
        path: &str,
    ) {
        let expected_location = match surface {
            ActionSurface::List(data_binding_id) => {
                let list_location = matches!(
                    placement.location,
                    WidgetActionLocation::Row | WidgetActionLocation::Selection
                );
                if list_location
                    && placement.selection_binding_id.as_deref() != Some(data_binding_id.as_str())
                {
                    self.error(
                        path,
                        "row and selection actions must name the containing list binding",
                    );
                }
                list_location
            }
            ActionSurface::Detail(data_binding_id) => {
                if let Some(binding) = self.action_bindings.get(placement.binding_id.as_str())
                    && binding
                        .inputs
                        .record_binding_id
                        .as_deref()
                        .is_some_and(|record| record != data_binding_id.as_str())
                {
                    self.error(
                        path,
                        "detail action input.record must match its containing detail binding",
                    );
                }
                placement.location == WidgetActionLocation::Detail
            }
            ActionSurface::Form(form_id) => {
                let footer_location = placement.location == WidgetActionLocation::FormFooter
                    && placement.form_id.is_none();
                if self
                    .forms
                    .get(form_id.as_str())
                    .is_some_and(|form| form.submit_binding_id == placement.binding_id)
                {
                    self.error(
                        path,
                        "a form cannot place its own submit action as a secondary footer action",
                    );
                }
                if let Some(binding) = self.action_bindings.get(placement.binding_id.as_str())
                    && binding
                        .inputs
                        .draft_form_id
                        .as_deref()
                        .is_some_and(|draft| draft != form_id)
                {
                    self.error(
                        path,
                        "form footer draft input must name its containing form",
                    );
                }
                footer_location
            }
            ActionSurface::Page => placement.location == WidgetActionLocation::Toolbar,
        };
        if !expected_location {
            self.error(
                path,
                "action location must match its containing page, list, detail, or form surface",
            );
        }
    }

    fn check_children(&mut self, node: &WidgetNode, path: &str) {
        let valid = match &node.component {
            WidgetComponent::Section { .. }
            | WidgetComponent::Detail { .. }
            | WidgetComponent::Dialog { .. } => true,
            WidgetComponent::List { .. } => node.children.iter().all(|child| {
                matches!(
                    &child.component,
                    WidgetComponent::Action { .. } | WidgetComponent::Navigation { .. }
                )
            }),
            WidgetComponent::Form { .. } => node.children.iter().all(|child| {
                matches!(
                    &child.component,
                    WidgetComponent::Field { .. }
                        | WidgetComponent::Action { .. }
                        | WidgetComponent::ChildGrid { .. }
                )
            }),
            WidgetComponent::Field { .. }
            | WidgetComponent::ChildGrid { .. }
            | WidgetComponent::Action { .. }
            | WidgetComponent::Navigation { .. }
            | WidgetComponent::Composition { .. } => node.children.is_empty(),
        };
        if !valid {
            self.error(
                &format!("{path}/children"),
                "component children do not match the closed tree composition rules",
            );
        }
        if matches!(&node.component, WidgetComponent::Form { .. }) {
            for child in &node.children {
                if let WidgetComponent::Action { placement_id } = &child.component
                    && !self
                        .placements
                        .get(placement_id.as_str())
                        .is_some_and(|placement| {
                            placement.location == WidgetActionLocation::FormFooter
                        })
                {
                    self.error(
                        &format!("{path}/children"),
                        "form children may contain only secondary form_footer actions",
                    );
                }
            }
        }
    }

    fn include_property(&mut self, property: &norn_ir::PropertyRef, path: &str) {
        if self.resolve_property(property, path).is_some() {
            self.included_fields
                .insert((property.binding_id.clone(), property.property_id.clone()));
            let presentations: Vec<(String, Option<String>)> = self
                .ux
                .fields
                .iter()
                .filter_map(|field| match &field.target {
                    WidgetFieldTarget::Property { property: target }
                        if target.binding_id == property.binding_id
                            && target.property_id == property.property_id =>
                    {
                        Some((
                            field.field_id.clone(),
                            field.option_query_binding_id.clone(),
                        ))
                    }
                    WidgetFieldTarget::Property { .. }
                    | WidgetFieldTarget::FormInput { .. }
                    | WidgetFieldTarget::ParentProperty { .. }
                    | WidgetFieldTarget::ChildRowProperty { .. } => None,
                })
                .collect();
            if presentations.len() != 1 {
                self.error(
                    path,
                    "explicitly included property must have exactly one field presentation",
                );
            } else if let Some((field_id, option_binding_id)) = presentations.first() {
                let value_type = self
                    .resolve_property(property, path)
                    .and_then(property_type);
                let option_type = option_binding_id
                    .as_deref()
                    .and_then(|id| self.option_binding_key_type(id, path));
                self.register_semantic_node(field_id, value_type, true, false, option_type, path);
            }
        }
    }

    fn register_semantic_node(
        &mut self,
        id: &str,
        value_type: Option<Type>,
        control_target: bool,
        action_target: bool,
        option_key_type: Option<Type>,
        path: &str,
    ) {
        if let Some(mut existing) = self.semantic_nodes.remove(id) {
            let is_local = self.local_semantic_ids.contains(id);
            if !is_local && (existing.control_target || existing.action_target) {
                let same = existing.value_type == value_type
                    && existing.control_target == control_target
                    && existing.action_target == action_target
                    && existing.option_key_type == option_key_type;
                self.semantic_nodes.insert(id.to_owned(), existing);
                if same {
                    return;
                }
                self.error(
                    path,
                    "semantic component id resolves to conflicting field definitions",
                );
                return;
            }
            if is_local && (existing.control_target || existing.action_target) {
                let same = (existing.value_type.is_none() || existing.value_type == value_type)
                    && existing.control_target == control_target
                    && existing.action_target == action_target;
                if same {
                    existing.value_type = value_type;
                    existing.option_key_type = option_key_type;
                    self.semantic_nodes.insert(id.to_owned(), existing);
                    return;
                }
                self.semantic_nodes.insert(id.to_owned(), existing);
                self.error(
                    path,
                    "semantic component id resolves to conflicting field definitions",
                );
                return;
            }
            self.semantic_nodes.insert(id.to_owned(), existing);
            self.error(
                path,
                if is_local {
                    "field id collides with a non-field semantic component id"
                } else {
                    "field id collides with another semantic component id"
                },
            );
            return;
        }
        self.semantic_nodes.insert(
            id.to_owned(),
            SemanticNode {
                value_type,
                option_key_type,
                control_target,
                action_target,
                form_input_initialization: FormInputInitialization::default(),
            },
        );
    }

    fn set_form_input_initialization(
        &mut self,
        id: &str,
        initialization: &FormInputInitialization,
    ) {
        if let Some(node) = self.semantic_nodes.get_mut(id) {
            node.form_input_initialization = initialization.clone();
        }
    }

    fn check_display_initialization(
        &self,
        control: WidgetFieldControl,
        initialization: &FormInputInitialization,
        path: &str,
    ) {
        if control == WidgetFieldControl::Display
            && initialization.required
            && !initialization.source_available
        {
            self.error(
                path,
                "required display form input must have a valid initial record value",
            );
        }
    }

    fn check_control_type(
        &mut self,
        control: WidgetFieldControl,
        format: Option<WidgetFieldFormat>,
        option_binding: Option<&str>,
        value_type: &Type,
        path: &str,
    ) {
        let options = option_binding.and_then(|id| self.option_binding_key_type(id, path));
        self.check_control_type_with_option_type(
            control,
            format,
            options.as_ref(),
            value_type,
            path,
        );
    }

    fn check_control_type_with_option_type(
        &self,
        control: WidgetFieldControl,
        format: Option<WidgetFieldFormat>,
        option_key_type: Option<&Type>,
        value_type: &Type,
        path: &str,
    ) {
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
        )]
        let value_type = match value_type {
            Type::Optional(inner) => inner.as_ref(),
            value_type => value_type,
        };
        let textual = matches!(value_type, Type::Text { .. } | Type::Enum { .. });
        let numeric = matches!(
            value_type,
            Type::Integer | Type::Number | Type::Decimal { .. }
        );
        let date = matches!(value_type, Type::Date);
        let datetime = matches!(value_type, Type::Timestamp { .. });
        let boolean = matches!(value_type, Type::Bool);
        let valid = match control {
            WidgetFieldControl::Text | WidgetFieldControl::TextArea => textual,
            WidgetFieldControl::Number => numeric,
            WidgetFieldControl::Date => date,
            WidgetFieldControl::DateTime => datetime,
            WidgetFieldControl::Checkbox => boolean,
            WidgetFieldControl::Select => option_key_type.map_or_else(
                || matches!(value_type, Type::Enum { .. }),
                |key| types_compatible(key, value_type) && types_compatible(value_type, key),
            ),
            WidgetFieldControl::Link => option_key_type.is_some_and(|key| {
                types_compatible(key, value_type) && types_compatible(value_type, key)
            }),
            WidgetFieldControl::Display => true,
        };
        if !valid {
            self.error(&format!("{path}/control"), "field control is incompatible with the resolved property type or missing its option query");
        }
        if let Some(format) = format {
            let compatible = match format {
                WidgetFieldFormat::Text => textual,
                WidgetFieldFormat::Integer => matches!(value_type, Type::Integer),
                WidgetFieldFormat::Decimal
                | WidgetFieldFormat::Currency
                | WidgetFieldFormat::Percent => numeric,
                WidgetFieldFormat::Date => date,
                WidgetFieldFormat::DateTime => datetime,
                WidgetFieldFormat::Boolean => boolean,
                WidgetFieldFormat::MetricBuckets => {
                    control == WidgetFieldControl::Display && matches!(value_type, Type::Json)
                }
            };
            if !compatible {
                self.error(
                    &format!("{path}/format"),
                    "field format hint is incompatible with the property type",
                );
            }
        }
    }

    fn schema_control_type(&mut self, schema: &Schema) -> Type {
        let mut records = BTreeMap::new();
        self.schema_type(schema, &mut records)
    }

    fn option_binding_key_type(&mut self, binding_id: &str, path: &str) -> Option<Type> {
        if !self.is_bounded_list_binding(binding_id) {
            self.error(
                &format!("{path}/optionQueryBindingId"),
                "option query must be a bounded list or traversal binding",
            );
            return None;
        }
        let binding = self.bindings.get(binding_id).copied()?.clone();
        let artifact = self.resolve_ref(
            &binding.object.id,
            &binding.object.version,
            &ArtifactKind::Entity,
            &format!("{path}/optionQueryBindingId/object"),
        )?;
        let Some(Ok(object)) = self.catalog.object_type(&artifact.artifact_id) else {
            self.error(
                &format!("{path}/optionQueryBindingId"),
                "option binding entity type is unavailable",
            );
            return None;
        };
        let [identity_key] = object.keys.as_slice() else {
            self.error(
                &format!("{path}/optionQueryBindingId"),
                "option binding requires exactly one single-property identity key",
            );
            return None;
        };
        let [property_id] = identity_key.components.as_slice() else {
            self.error(
                &format!("{path}/optionQueryBindingId"),
                "option binding requires exactly one single-property identity key",
            );
            return None;
        };
        let Some(property) = object.property(property_id.as_str()) else {
            self.error(
                &format!("{path}/optionQueryBindingId"),
                "option identity property does not resolve on its entity",
            );
            return None;
        };
        property_type(property)
    }

    fn check_actions(&mut self) {
        let bindings = self.ux.action_bindings.clone();
        for (index, binding) in bindings.iter().enumerate() {
            let path = format!("/payload/ux/actionBindings/{index}");
            if binding.required_capability.trim().is_empty() {
                self.error(
                    &format!("{path}/requiredCapability"),
                    "required capability must not be blank",
                );
            }
            let Some(action_artifact) = self.resolve_ref(
                &binding.action.id,
                &binding.action.version,
                &ArtifactKind::Action,
                &format!("{path}/action"),
            ) else {
                continue;
            };
            let ArtifactPayload::Action(payload) = &action_artifact.payload else {
                continue;
            };
            let Some(root) = self.effect_action(
                self.owner,
                self.ux,
                &binding.binding_id,
                &format!("{path}/action"),
            ) else {
                continue;
            };
            let Some(raw_schema) = payload.input_schema.as_value() else {
                self.error(
                    &format!("{path}/action"),
                    "ActionIR input schema is missing",
                );
                continue;
            };
            let schema = match Schema::parse_action(raw_schema, "$.inputSchema") {
                Ok(schema) => schema,
                Err(error) => {
                    self.error(
                        &format!("{path}/action"),
                        format!("ActionIR input schema is invalid: {}", error.reason),
                    );
                    continue;
                }
            };
            let Some(fields) = schema.properties() else {
                self.error(
                    &format!("{path}/action"),
                    "ActionIR input schema must be a closed object",
                );
                continue;
            };
            let required = schema.required().cloned().unwrap_or_default();
            let parsed = ParsedAction {
                fields: fields
                    .iter()
                    .map(|(name, schema)| (name.clone(), (schema.clone(), required.contains(name))))
                    .collect(),
                root,
            };
            self.actions
                .insert(binding.binding_id.clone(), parsed.clone());
        }
        for (index, binding) in bindings.iter().enumerate() {
            let path = format!("/payload/ux/actionBindings/{index}/inputMapping");
            let input_env = self.expression_env(
                &binding.inputs,
                &format!("/payload/ux/actionBindings/{index}"),
            );
            if let Some(action) = self.actions.get(binding.binding_id.as_str()).cloned() {
                self.check_action_mapping(binding, &action, input_env.as_ref(), &path);
            }
        }
    }

    fn check_action_mapping(
        &mut self,
        binding: &WidgetUxActionBinding,
        action: &ParsedAction,
        input_env: Option<&ZenTypeEnv>,
        path: &str,
    ) {
        for (name, (schema, required)) in &action.fields {
            if let Some(expression) = binding.input_mapping.get(name) {
                self.check_mapped_expression(
                    expression,
                    schema,
                    *required,
                    binding,
                    name,
                    action,
                    input_env,
                    &format!("{path}/{name}"),
                );
            }
        }
        for name in binding.input_mapping.keys() {
            if !action.fields.contains_key(name) {
                self.error(
                    &format!("{path}/{name}"),
                    "mapping names an input not declared by ActionIR",
                );
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mapping admission needs the exact action field, form binding, expected schema, expression environment, and diagnostic path"
    )]
    fn check_mapped_expression(
        &mut self,
        source: &norn_ir::ZenExpression,
        schema: &Schema,
        required: bool,
        binding: &WidgetUxActionBinding,
        action_field_name: &str,
        action: &ParsedAction,
        env: Option<&ZenTypeEnv>,
        path: &str,
    ) {
        let Some(env) = env else {
            return;
        };
        let expression = match Expression::compile(source.as_str()) {
            Ok(expression) => expression,
            Err(error) => {
                self.error(path, format!("ZEN mapping does not compile: {error}"));
                return;
            }
        };
        // Parent/row mutations intentionally carry their bounded canonical
        // payloads as opaque JSON leaves. Permit only whole-value forwarding
        // from the matching declared form draft member; member access or
        // expression composition remains subject to normal ZEN type checks.
        if self.is_parent_rows_draft_json_mapping(
            source,
            schema,
            required,
            binding,
            action_field_name,
            action,
        ) {
            return;
        }
        // Empty lists are type-correct for any array item schema, but the
        // expression checker needs an expected `List<T>` to infer their
        // element type. Their shape is exact and they have no members to
        // validate, so admit this literal for array and JSON inputs directly.
        if source.as_str().trim() == "[]" && schema_accepts_empty_array(schema) {
            return;
        }
        // Let the expression checker infer the exact constructed record shape;
        // ActionIR schemas are then checked recursively below so optional
        // object members need not be fabricated in every expression.
        match check_expression_with_sources(
            &expression,
            env,
            &Type::Json,
            &norn_ir::ResourceLimitProfile::norn_limits_v1(),
        ) {
            Ok((checked, construction)) => {
                // InferredType intentionally erases Integer versus Decimal.
                // Check every constructed integer leaf against its own source,
                // retaining all members when list inference joins their types.
                let sources = construction.as_ref().map(|source| vec![source]);
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
                )]
                let inferred = match checked.inferred() {
                    InferredType::Optional(actual)
                        if !required
                            && self.is_optional_draft_member_mapping(source, &binding.inputs) =>
                    {
                        actual.as_ref()
                    }
                    inferred => inferred,
                };
                let mut integer_proof = |sources: &[&ConstructionSource], leaf_path: &str| {
                    !sources.is_empty()
                        && sources.iter().all(|source| {
                            matches!(source, ConstructionSource::Scalar(source)
                                if self.integer_source_proven(source, &binding.inputs, leaf_path))
                        })
                };
                let schema_result = inferred_matches_schema(
                    inferred,
                    schema,
                    sources.as_deref(),
                    &mut integer_proof,
                    path,
                );
                if let Err(error) = schema_result {
                    self.error(path, error);
                }
            }
            Err(errors) => {
                for error in errors {
                    self.error(
                        path,
                        format!("ZEN mapping has incompatible type: {}", error.message),
                    );
                }
            }
        }
    }

    fn is_parent_rows_draft_json_mapping(
        &self,
        source: &norn_ir::ZenExpression,
        schema: &Schema,
        required: bool,
        binding: &WidgetUxActionBinding,
        action_field_name: &str,
        action: &ParsedAction,
    ) -> bool {
        if !required || !matches!(schema, Schema::Json) {
            return false;
        }
        let Some(ActionEffectProgram::MutateParentWithRows {
            parent_values_field,
            row_ops_field,
            ..
        }) = action.root.effect_program.as_ref()
        else {
            return false;
        };
        if action_field_name != parent_values_field && action_field_name != row_ops_field {
            return false;
        }
        if binding.input_mapping.get(action_field_name) != Some(source) {
            return false;
        }
        if source.as_str().trim() != format!("input.draft.{action_field_name}") {
            return false;
        }
        let Some(form_id) = binding.inputs.draft_form_id.as_deref() else {
            return false;
        };
        self.forms.get(form_id).is_some_and(|form| {
            form.submit_binding_id == binding.binding_id
                && form
                    .input_names
                    .iter()
                    .any(|input_name| input_name == action_field_name)
        })
    }

    fn is_optional_draft_member_mapping(
        &self,
        source: &norn_ir::ZenExpression,
        inputs: &WidgetExpressionInputs,
    ) -> bool {
        let mut parts = source.as_str().trim().split('.');
        if parts.next() != Some("input") || parts.next() != Some("draft") {
            return false;
        }
        let Some(name) = parts.next() else {
            return false;
        };
        if parts.next().is_some() {
            return false;
        }
        let Some(form_id) = inputs.draft_form_id.as_deref() else {
            return false;
        };
        let Some(form) = self.forms.get(form_id) else {
            return false;
        };
        if !form.input_names.iter().any(|input_name| input_name == name) {
            return false;
        }
        let Some(binding) = self.action_bindings.get(form.submit_binding_id.as_str()) else {
            return false;
        };
        self.actions
            .get(binding.binding_id.as_str())
            .and_then(|action| action.fields.get(name))
            .is_some_and(|(_, required)| !required)
    }

    fn integer_source_proven(
        &mut self,
        source: &str,
        inputs: &WidgetExpressionInputs,
        path: &str,
    ) -> bool {
        self.direct_input_type(source, inputs, path)
            .as_ref()
            .is_some_and(type_is_integer_source)
            || norn_ir::Decimal::parse(source.trim()).is_ok_and(|value| value.is_integer())
    }

    fn direct_input_type(
        &mut self,
        source: &str,
        inputs: &WidgetExpressionInputs,
        path: &str,
    ) -> Option<Type> {
        let trimmed = source.trim();
        let mut parts = trimmed.split('.');
        if parts.next()? != "input" {
            return None;
        }
        let source_kind = parts.next()?;
        let member = parts.next();
        if parts.next().is_some() {
            return None;
        }
        if source_kind == "recordKey" || source_kind == "recordVersion" {
            inputs.record_binding_id.as_ref()?;
            return Some(Type::text());
        }
        let member = member?;
        match source_kind {
            "record" => {
                let binding_id = inputs.record_binding_id.as_deref()?;
                let property = norn_ir::PropertyRef {
                    binding_id: binding_id.to_owned(),
                    property_id: member.to_owned(),
                };
                self.resolve_property(&property, path)
                    .and_then(property_type)
            }
            "context" => {
                let page_id = inputs.context_page_id.as_deref()?;
                let page = self.pages.get(page_id)?;
                let param = page
                    .context_params
                    .iter()
                    .find(|param| param.param_id == member)?;
                Some(value_type(param.value_type, param.required))
            }
            "draft" => {
                let form_id = inputs.draft_form_id.as_deref()?;
                let form = (*self.forms.get(form_id)?).clone();
                let binding = (*self.action_bindings.get(form.submit_binding_id.as_str())?).clone();
                let (schema, required) = self
                    .actions
                    .get(binding.binding_id.as_str())?
                    .fields
                    .get(member)?
                    .clone();
                let mut records = BTreeMap::new();
                let mut type_ = self.schema_type(&schema, &mut records);
                if !required {
                    type_ = type_.optional();
                }
                Some(type_)
            }
            _ => None,
        }
    }

    fn action_for_form(&self, form: &WidgetForm) -> Option<&ParsedAction> {
        let binding = self.action_bindings.get(form.submit_binding_id.as_str())?;
        self.actions.get(binding.binding_id.as_str())
    }

    fn parent_object(&self, action: &ParsedAction) -> Option<EffectiveObjectType> {
        let ActionEffectProgram::MutateParentWithRows {
            entity_artifact_id, ..
        } = action.root.effect_program.as_ref()?
        else {
            return None;
        };
        self.catalog.object_type(entity_artifact_id)?.ok().cloned()
    }

    fn child_object(
        &self,
        action: &ParsedAction,
        collection_id: &str,
    ) -> Option<EffectiveObjectType> {
        let parent = self.parent_object(action)?;
        let collection = parent
            .owned_collections
            .iter()
            .find(|candidate| candidate.id.as_str() == collection_id)?;
        self.catalog
            .object_type_named(collection.child_type.as_str())?
            .ok()
            .cloned()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This pass validates curated form inputs and their submit ActionIR contract as one unit."
    )]
    fn check_forms(&mut self) {
        let forms = self.ux.forms.clone();
        for (index, form) in forms.iter().enumerate() {
            let path = format!("/payload/ux/forms/{index}");
            self.check_generated_transition_condition(&form.submit_binding_id, &path);
            if form.submit.label.0.is_empty()
                || form
                    .submit
                    .label
                    .0
                    .values()
                    .any(|label| label.trim().is_empty())
            {
                self.error(
                    &format!("{path}/submit/label"),
                    "form submit label must contain nonblank localized text",
                );
            }
            self.check_confirmation(
                &form.submit.confirmation,
                &format!("{path}/submit/confirmation"),
            );
            let Some(binding) = self
                .action_bindings
                .get(form.submit_binding_id.as_str())
                .copied()
            else {
                self.error(
                    &format!("{path}/submitBindingId"),
                    "form submit action binding does not exist",
                );
                continue;
            };
            let input_mapping = binding.input_mapping.clone();
            let draft_form_id = binding.inputs.draft_form_id.clone();
            let record_binding_id = binding.inputs.record_binding_id.clone();
            let Some(action) = self.actions.get(binding.binding_id.as_str()).cloned() else {
                self.error(
                    &format!("{path}/submitBindingId"),
                    "form submit ActionIR schema could not be resolved",
                );
                continue;
            };
            self.check_parent_rows_form_contract(form, binding, &action, &path);
            for (name, (_, required)) in &action.fields {
                if *required && !input_mapping.contains_key(name) {
                    self.error(
                        &format!("{path}/submitBindingId"),
                        format!("required submit input {name:?} has no action binding mapping"),
                    );
                }
            }
            let form_binding = match form.mode {
                WidgetFormMode::Edit => {
                    let Some(data_binding_id) = &form.data_binding_id else {
                        self.error(
                            &format!("{path}/dataBindingId"),
                            "edit form requires an object-detail binding",
                        );
                        continue;
                    };
                    if !self
                        .bindings
                        .get(data_binding_id.as_str())
                        .is_some_and(|data| {
                            matches!(
                                &data.query,
                                WidgetDataQuery::ObjectDetail { .. }
                                    | WidgetDataQuery::ObjectRuntimeDetail { .. }
                            )
                        })
                    {
                        self.error(
                            &format!("{path}/dataBindingId"),
                            "edit form must use an object-detail binding",
                        );
                    }
                    Some(data_binding_id.as_str())
                }
                WidgetFormMode::Create | WidgetFormMode::ActionInput => {
                    if let Some(data_binding_id) = form.data_binding_id.as_deref() {
                        if !self.record_source_available_for_form(form, data_binding_id) {
                            self.error(&format!("{path}/dataBindingId"), "create and action_input form prefill requires a declared record or list-row source");
                        }
                        Some(data_binding_id)
                    } else {
                        None
                    }
                }
            };
            let mut names = BTreeSet::new();
            for (name_index, name) in form.input_names.iter().enumerate() {
                if name.trim().is_empty() || !names.insert(name.as_str()) {
                    self.error(
                        &format!("{path}/inputNames/{name_index}"),
                        "form input names must be nonblank and unique",
                    );
                }
                if !action.fields.contains_key(name) {
                    self.error(
                        &format!("{path}/inputNames/{name_index}"),
                        "form input name is not declared by its submit ActionIR",
                    );
                }
                if input_mapping.contains_key(name) {
                    let expected = format!("input.draft.{name}");
                    if input_mapping
                        .get(name)
                        .is_none_or(|expression| expression.as_str().trim() != expected.as_str())
                    {
                        self.error(
                            &format!("{path}/inputNames/{name_index}"),
                            "form input must map directly from its matching input.draft member",
                        );
                    }
                } else {
                    self.error(
                        &format!("{path}/inputNames/{name_index}"),
                        "form input has no action input mapping",
                    );
                }
            }
            if (!form.input_names.is_empty() || draft_form_id.is_some())
                && draft_form_id.as_deref() != Some(form.form_id.as_str())
            {
                self.error(
                    &format!("{path}/submitBindingId"),
                    "submit action binding must explicitly declare this form as input.draft",
                );
            }
            if let Some(version) = &form.version_conflict_binding {
                if form.mode != WidgetFormMode::Edit {
                    self.error(
                        &format!("{path}/versionConflictBinding"),
                        "version conflict binding is only valid on an edit form",
                    );
                }
                let Some(data_binding_id) = form_binding else {
                    continue;
                };
                if version.property.binding_id != data_binding_id {
                    self.error(
                        &format!("{path}/versionConflictBinding/property"),
                        "version property must belong to the edited object binding",
                    );
                }
                let Some(property) = self.resolve_property(
                    &version.property,
                    &format!("{path}/versionConflictBinding/property"),
                ) else {
                    continue;
                };
                let property_type_ = property_type(property);
                if property.nullable
                    || property.computed
                    || !property_type_
                        .as_ref()
                        .is_some_and(|ty| matches!(ty, Type::Integer | Type::Text { .. }))
                {
                    self.error(
                        &format!("{path}/versionConflictBinding/property"),
                        "version property must be a required, stored integer or text property",
                    );
                }
                let expected_source = format!("input.record.{}", version.property.property_id);
                if record_binding_id.as_deref() != Some(data_binding_id)
                    || input_mapping
                        .get(&version.input_name)
                        .is_none_or(|expr| expr.as_str().trim() != expected_source)
                {
                    self.error(
                        &format!("{path}/versionConflictBinding/inputName"),
                        "version input must map directly from the matching edited record property",
                    );
                }
                if !action.fields.contains_key(&version.input_name) {
                    self.error(
                        &format!("{path}/versionConflictBinding/inputName"),
                        "version input is not declared by the submit ActionIR",
                    );
                } else if form.input_names.contains(&version.input_name) {
                    self.error(&format!("{path}/versionConflictBinding/inputName"), "version input is internal and must not be included as an editable form field");
                } else if let Some((schema, required)) = action.fields.get(&version.input_name)
                    && (!required
                        || property_type_
                            .as_ref()
                            .is_none_or(|ty| !type_fits_schema(ty, schema)))
                {
                    self.error(
                        &format!("{path}/versionConflictBinding/inputName"),
                        "version input must be required and match the edited record's version type",
                    );
                }
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the parent and row inputs share one form submission contract and record snapshot"
    )]
    fn check_parent_rows_form_contract(
        &mut self,
        form: &WidgetForm,
        binding: &WidgetUxActionBinding,
        action: &ParsedAction,
        path: &str,
    ) {
        let Some(ActionEffectProgram::MutateParentWithRows {
            object_id_field,
            entity_artifact_id,
            entity_artifact_version,
            parent_values_field,
            row_ops_field,
            parent_version_field,
            ..
        }) = action.root.effect_program.as_ref()
        else {
            return;
        };

        if !matches!(form.mode, WidgetFormMode::Create | WidgetFormMode::Edit) {
            self.error(
                &format!("{path}/mode"),
                "parent-and-rows submit requires a create or edit form",
            );
        }
        for name in [parent_values_field, row_ops_field] {
            if !form.input_names.contains(name) {
                self.error(
                    &format!("{path}/inputNames"),
                    format!("parent-and-rows form must include action input {name:?}"),
                );
            }
        }
        if !direct_draft_mapping(binding, parent_values_field)
            || !direct_draft_mapping(binding, row_ops_field)
        {
            self.error(
                &format!("{path}/submitBindingId"),
                "parent values and row operations must map directly from their matching form draft members",
            );
        }
        if !action
            .fields
            .get(parent_values_field)
            .is_some_and(|(schema, _)| matches!(schema, Schema::Json))
        {
            self.error(
                &format!("{path}/submitBindingId"),
                "parent-values action input must use the bounded JSON leaf schema; the kernel validates its object shape",
            );
        }
        if !action
            .fields
            .get(row_ops_field)
            .is_some_and(|(schema, _)| matches!(schema, Schema::Json))
        {
            self.error(
                &format!("{path}/submitBindingId"),
                "row-operations action input must use the bounded JSON leaf schema; the kernel validates its operation array",
            );
        }

        match form.mode {
            WidgetFormMode::Edit => {
                let binding_matches =
                    form.data_binding_id.as_deref() == binding.inputs.record_binding_id.as_deref();
                if !binding_matches
                    || binding
                        .input_mapping
                        .get(object_id_field)
                        .is_none_or(|expression| expression.as_str().trim() != "input.recordKey")
                    || binding
                        .input_mapping
                        .get(parent_version_field)
                        .is_none_or(|expression| {
                            expression.as_str().trim() != "input.recordVersion"
                        })
                {
                    self.error(
                        &format!("{path}/submitBindingId"),
                        "edit submit must use its bound parent record key and authoritative record version",
                    );
                }
                let parent_binding_matches = form
                    .data_binding_id
                    .as_deref()
                    .and_then(|id| self.bindings.get(id))
                    .is_some_and(|parent_binding| {
                        parent_binding.object.id == *entity_artifact_id
                            && parent_binding.object.version == *entity_artifact_version
                            && matches!(
                                parent_binding.query,
                                WidgetDataQuery::ObjectDetail { .. }
                                    | WidgetDataQuery::ObjectRuntimeDetail { .. }
                            )
                    });
                if !parent_binding_matches {
                    self.error(
                        &format!("{path}/dataBindingId"),
                        "edit form must bind the exact parent entity detail whose version protects the mutation",
                    );
                }
            }
            WidgetFormMode::Create => {
                let id_is_draft = binding.input_mapping.get(object_id_field).is_some_and(|expression| {
                    let expected_prefix = "input.draft.";
                    expression
                        .as_str()
                        .trim()
                        .strip_prefix(expected_prefix)
                        .is_some_and(|name| {
                            name.bytes().enumerate().all(|(index, byte)| {
                                byte == b'_' || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
                            }) && form.input_names.iter().any(|input| input == name)
                                && self.ux.fields.iter().any(|field| {
                                    matches!(&field.target, WidgetFieldTarget::FormInput { form_id, input_name, .. }
                                        if form_id == &form.form_id && input_name == name)
                                })
                        })
                });
                let version_is_zero =
                    binding
                        .input_mapping
                        .get(parent_version_field)
                        .is_some_and(|expression| {
                            serde_json::from_str::<String>(expression.as_str().trim())
                                .is_ok_and(|value| value == "0")
                        });
                if !id_is_draft || !version_is_zero {
                    self.error(
                        &format!("{path}/submitBindingId"),
                        "create submit must use an explicit parent-id form input and parent version \"0\"",
                    );
                }
                self.check_parent_create_property_coverage(form, action, parent_values_field, path);
            }
            WidgetFormMode::ActionInput => {}
        }
    }

    fn check_parent_create_property_coverage(
        &mut self,
        form: &WidgetForm,
        action: &ParsedAction,
        parent_values_field: &str,
        path: &str,
    ) {
        let Some(parent) = self.parent_object(action) else {
            self.error(
                &format!("{path}/submitBindingId"),
                "create parent property coverage cannot be checked because the pinned parent entity is unavailable",
            );
            return;
        };
        for lifecycle in &parent.lifecycles {
            let property_id = lifecycle.lifecycle.property.as_str();
            let Some(property) = parent.property(property_id) else {
                continue;
            };
            if !property.nullable
                && !has_initial_lifecycle_default(property, &lifecycle.lifecycle.initial)
            {
                self.error(
                    &format!("{path}/submitBindingId"),
                    format!(
                        "create form requires lifecycle property {property_id:?} to declare a default in one of its initial states because the lifecycle field is read-only"
                    ),
                );
            }
        }
        for property in parent.properties.iter().filter(|property| {
            !property.nullable
                && !property.computed
                && property.default.is_none()
                && parent.lifecycle(property.id.as_str()).is_none()
        }) {
            let presented = self.ux.fields.iter().any(|field| {
                matches!(
                    &field.target,
                    WidgetFieldTarget::ParentProperty {
                        form_id,
                        input_name,
                        property_id,
                    } if form_id == &form.form_id
                        && input_name == parent_values_field
                        && property_id == property.id.as_str()
                ) && self.used_field_ids.contains(&field.field_id)
                    && field.control != WidgetFieldControl::Display
                    && field.visible_when.is_none()
                    && field.read_only_when.is_none()
            });
            if !presented {
                self.error(
                    &format!("{path}/submitBindingId"),
                    format!(
                        "create form must expose required parent property {:?} as an unconditional editable field or supply a declared default",
                        property.id.as_str()
                    ),
                );
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This pass validates placement contexts, confirmation, and merged ActionIR inputs together."
    )]
    fn check_placements(&mut self) {
        let placements = self.ux.action_placements.clone();
        for (index, placement) in placements.iter().enumerate() {
            let path = format!("/payload/ux/actionPlacements/{index}");
            self.check_generated_transition_condition(&placement.binding_id, &path);
            if placement.label.0.is_empty()
                || placement
                    .label
                    .0
                    .values()
                    .any(|label| label.trim().is_empty())
            {
                self.error(
                    &format!("{path}/label"),
                    "action placement label must contain nonblank localized text",
                );
            }
            let action_binding = self
                .action_bindings
                .get(placement.binding_id.as_str())
                .copied()
                .cloned();
            if action_binding.is_none() {
                self.error(
                    &format!("{path}/bindingId"),
                    "action placement names an unknown action binding",
                );
            }
            if let Some(form_id) = &placement.form_id {
                let pages = self.pages_at(&path);
                if pages.is_empty()
                    || self.rendered_forms.get(form_id).is_none_or(|rendered| {
                        pages
                            .iter()
                            .any(|page| page.is_none() || !rendered.contains(page))
                    })
                {
                    self.error(
                        &format!("{path}/formId"),
                        "form opener requires a matching Form node on every page where it is rendered",
                    );
                }
                if let Some(form) = self.forms.get(form_id.as_str()) {
                    if form.submit_binding_id != placement.binding_id {
                        self.error(
                            &format!("{path}/formId"),
                            "a form-opening placement must use the form's submit action binding",
                        );
                    }
                    if action_binding
                        .as_ref()
                        .and_then(|binding| binding.inputs.record_binding_id.as_deref())
                        != form.data_binding_id.as_deref()
                    {
                        self.error(&format!("{path}/formId"), "form opener record snapshot must match the form's declared data binding");
                    }
                    if placement.confirmation.severity != WidgetActionSeverity::Safe
                        || placement.confirmation.strength
                            != norn_ir::WidgetConfirmationStrength::None
                        || placement.confirmation.message.is_some()
                        || placement.confirmation.phrase.is_some()
                    {
                        self.error(&format!("{path}/confirmation"), "a form-opening placement is a safe, unconfirmed opener; confirmation belongs to form submission");
                    }
                    if placement.location == WidgetActionLocation::FormFooter {
                        self.error(
                            &format!("{path}/formId"),
                            "form_footer placements are secondary actions and cannot open a form",
                        );
                    }
                } else {
                    self.error(
                        &format!("{path}/formId"),
                        "action placement names an unknown form",
                    );
                }
            }
            if placement
                .group
                .as_ref()
                .is_some_and(|group| group.trim().is_empty())
            {
                self.error(&format!("{path}/group"), "action group must not be blank");
            }
            if !placement.selection_input_mapping.is_empty()
                && placement.location != WidgetActionLocation::Selection
            {
                self.error(
                    &format!("{path}/selectionInputMapping"),
                    "selection input mappings are only valid on selection placements",
                );
            }
            if matches!(
                placement.location,
                WidgetActionLocation::Row | WidgetActionLocation::Selection
            ) {
                let Some(selection) = placement.selection_binding_id.as_deref() else {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "row and selection actions require a list binding",
                    );
                    continue;
                };
                if !self.is_bounded_list_binding(selection) {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "action selection source must be a bounded list or traversal binding",
                    );
                }
                if placement.location == WidgetActionLocation::Selection
                    && self
                        .list_selections
                        .get(selection)
                        .is_none_or(|mode| *mode == WidgetSelectionMode::None)
                {
                    self.error(&format!("{path}/selectionBindingId"), "selection actions require a list with single or multiple selection enabled");
                }
                if placement.location == WidgetActionLocation::Row
                    && action_binding.as_ref().is_some_and(|binding| {
                        binding
                            .inputs
                            .record_binding_id
                            .as_deref()
                            .is_some_and(|record| record != selection)
                    })
                {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "row action input.record must name its containing list binding",
                    );
                }
                if placement.location == WidgetActionLocation::Selection
                    && action_binding.as_ref().is_some_and(|binding| {
                        binding
                            .inputs
                            .record_binding_id
                            .as_deref()
                            .is_some_and(|record| record != selection)
                    })
                {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "selection action input.record must name its selected list binding",
                    );
                }
                if let Some(binding_id) = action_binding
                    .as_ref()
                    .and_then(|binding| binding.inputs.record_binding_id.as_deref())
                    .filter(|binding_id| self.list_row_contexts.contains(*binding_id))
                {
                    let contextual_record = (placement.location == WidgetActionLocation::Row
                        && binding_id == selection)
                        || (placement.location == WidgetActionLocation::Selection
                            && binding_id == selection
                            && self.list_selections.get(selection)
                                == Some(&WidgetSelectionMode::Single));
                    if !contextual_record {
                        self.error(&format!("{path}/selectionBindingId"), "a list-row record input is available only to its row action or single-selection action");
                    }
                }
                if placement.location == WidgetActionLocation::Selection
                    && action_binding.as_ref().is_some_and(|binding| {
                        binding
                            .inputs
                            .selection_binding_id
                            .as_deref()
                            .is_some_and(|declared| declared != selection)
                    })
                {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "selection action input.selection must name the placement's list binding",
                    );
                }
                if placement.location != WidgetActionLocation::Selection
                    && action_binding
                        .as_ref()
                        .is_some_and(|binding| binding.inputs.selection_binding_id.is_some())
                {
                    self.error(
                        &format!("{path}/selectionBindingId"),
                        "input.selection is only available to a selection placement",
                    );
                }
            } else if placement.selection_binding_id.is_some()
                || !placement.selection_input_mapping.is_empty()
            {
                self.error(
                    &format!("{path}/selectionBindingId"),
                    "selection inputs are supported only on row or selection placements",
                );
            }
            self.check_confirmation(&placement.confirmation, &format!("{path}/confirmation"));
            self.check_optional_condition(
                placement.enabled_when.as_ref(),
                &format!("{path}/enabledWhen"),
            );
            if let (Some(binding), Some(action)) = (
                action_binding.as_ref(),
                self.actions.get(placement.binding_id.as_str()).cloned(),
            ) {
                let mut inputs = binding.inputs.clone();
                if placement.selection_binding_id.is_some() {
                    inputs
                        .selection_binding_id
                        .clone_from(&placement.selection_binding_id);
                }
                let input_env = if placement.selection_input_mapping.is_empty() {
                    None
                } else {
                    self.expression_env(&inputs, &path)
                };
                let mut placement_binding = binding.clone();
                placement_binding.inputs.clone_from(&inputs);
                for (name, expression) in &placement.selection_input_mapping {
                    if binding.input_mapping.contains_key(name) {
                        self.error(
                            &format!("{path}/selectionInputMapping/{name}"),
                            "selection and action binding mappings must use disjoint input names",
                        );
                        continue;
                    }
                    if let Some((schema, required)) = action.fields.get(name) {
                        self.check_mapped_expression(
                            expression,
                            schema,
                            *required,
                            &placement_binding,
                            name,
                            &action,
                            input_env.as_ref(),
                            &format!("{path}/selectionInputMapping/{name}"),
                        );
                    } else {
                        self.error(
                            &format!("{path}/selectionInputMapping/{name}"),
                            "selection mapping names an input not declared by ActionIR",
                        );
                    }
                }
                if placement.form_id.is_none() {
                    for (name, (_, required)) in &action.fields {
                        if *required
                            && !binding.input_mapping.contains_key(name)
                            && !placement.selection_input_mapping.contains_key(name)
                        {
                            self.error(&format!("{path}/selectionInputMapping"), format!("required ActionIR input {name:?} has no mapping for this placement"));
                        }
                    }
                }
            }
        }
        let placement_ids: Vec<String> =
            self.placements.keys().map(|id| (*id).to_owned()).collect();
        for placement_id in placement_ids {
            if !self.referenced_placements.contains(&placement_id) {
                self.error(
                    "/payload/ux/actionPlacements",
                    format!("placement {placement_id:?} is not present in a page component tree"),
                );
            }
        }
    }

    fn check_confirmation(&self, confirmation: &WidgetActionConfirmation, path: &str) {
        if confirmation.message.as_ref().is_some_and(|text| {
            text.0.is_empty() || text.0.values().any(|value| value.trim().is_empty())
        }) {
            self.error(
                &format!("{path}/message"),
                "confirmation message values must not be blank",
            );
        }
        let min_strength = match confirmation.severity {
            WidgetActionSeverity::Safe => 0,
            WidgetActionSeverity::Confirm | WidgetActionSeverity::Destructive => 1,
            WidgetActionSeverity::Irreversible => 2,
        };
        let strength = match confirmation.strength {
            norn_ir::WidgetConfirmationStrength::None => 0,
            norn_ir::WidgetConfirmationStrength::Click => 1,
            norn_ir::WidgetConfirmationStrength::Typed => 2,
        };
        if strength < min_strength {
            self.error(&format!("{path}/strength"), "confirm and destructive actions need click confirmation; irreversible actions need typed confirmation");
        }
        match (&confirmation.strength, &confirmation.phrase) {
            (norn_ir::WidgetConfirmationStrength::Typed, Some(phrase))
                if !phrase.0.is_empty()
                    && phrase.0.values().all(|value| !value.trim().is_empty()) => {}
            (norn_ir::WidgetConfirmationStrength::Typed, _) => self.error(
                &format!("{path}/phrase"),
                "typed confirmation requires nonblank localized confirmation text",
            ),
            (_, Some(_)) => self.error(
                &format!("{path}/phrase"),
                "confirmation phrase is only valid for typed confirmation",
            ),
            _ => {}
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This pass checks app routes and each navigation destination against its page context."
    )]
    fn check_navigation_and_app(&mut self) {
        let navigation_items = self.ux.navigation_items.clone();
        for (index, item) in navigation_items.iter().enumerate() {
            let path = format!("/payload/ux/navigationItems/{index}");
            if item.required_capability.trim().is_empty() {
                self.error(
                    &format!("{path}/requiredCapability"),
                    "navigation capability requirement must not be blank",
                );
            }
            if item.label.0.is_empty() || item.label.0.values().any(|label| label.trim().is_empty())
            {
                self.error(
                    &format!("{path}/label"),
                    "navigation label must contain nonblank localized text",
                );
            }
            self.check_optional_condition(
                item.visible_when.as_ref(),
                &format!("{path}/visibleWhen"),
            );
            match &item.destination {
                WidgetNavigationDestination::Page {
                    page_id,
                    parameters,
                } => {
                    let Some(page) = self.pages.get(page_id.as_str()).map(|page| (*page).clone())
                    else {
                        self.error(
                            &format!("{path}/destination/pageId"),
                            "navigation destination page does not exist",
                        );
                        continue;
                    };
                    let record_contexts =
                        self.navigation_record_contexts(item.item_id.as_str(), &path);
                    self.check_context_parameters(
                        &page,
                        parameters,
                        &record_contexts,
                        &format!("{path}/destination/parameters"),
                    );
                }
                WidgetNavigationDestination::Object {
                    object,
                    data_binding_id,
                } => {
                    self.resolve_ref(
                        &object.id,
                        &object.version,
                        &ArtifactKind::Entity,
                        &format!("{path}/destination/object"),
                    );
                    let Some(binding) = self
                        .bindings
                        .get(data_binding_id.as_str())
                        .copied()
                        .cloned()
                    else {
                        self.error(
                            &format!("{path}/destination/dataBindingId"),
                            "object destination binding does not exist",
                        );
                        continue;
                    };
                    if binding.object != *object {
                        self.error(&format!("{path}/destination/object"), "object destination must match the exact entity artifact of its data binding");
                    }
                    if !matches!(
                        &binding.query,
                        WidgetDataQuery::ObjectDetail { .. }
                            | WidgetDataQuery::ObjectRuntimeDetail { .. }
                    ) {
                        self.error(
                            &format!("{path}/destination/dataBindingId"),
                            "object destination requires an object-detail binding",
                        );
                    }
                }
                WidgetNavigationDestination::NativeProjection {
                    data_binding_id, ..
                } => {
                    if !self
                        .bindings
                        .get(data_binding_id.as_str())
                        .is_some_and(|binding| {
                            matches!(
                                &binding.query,
                                WidgetDataQuery::ObjectDetail { .. }
                                    | WidgetDataQuery::ObjectRuntimeDetail { .. }
                            )
                        })
                    {
                        self.error(
                            &format!("{path}/destination/dataBindingId"),
                            "native projection requires an object-detail binding",
                        );
                    }
                }
            }
        }
        let Some(app) = self.ux.app.clone() else {
            return;
        };
        let app_path = "/payload/ux/app";
        let mut app_pages = BTreeSet::new();
        for (index, reference) in app.page_refs.iter().enumerate() {
            if !app_pages.insert(reference.page_id.as_str()) {
                self.error(
                    &format!("{app_path}/pageRefs/{index}/pageId"),
                    "app page references must be unique",
                );
            }
            if !self.pages.contains_key(reference.page_id.as_str()) {
                self.error(
                    &format!("{app_path}/pageRefs/{index}/pageId"),
                    "app page reference does not exist in this widget",
                );
            }
        }
        if !self.pages.contains_key(app.entry_page_id.as_str())
            || !app_pages.contains(app.entry_page_id.as_str())
        {
            self.error(
                &format!("{app_path}/entryPageId"),
                "app entry page must be a declared page reference",
            );
        } else if self
            .pages
            .get(app.entry_page_id.as_str())
            .is_some_and(|page| page.context_params.iter().any(|param| param.required))
        {
            self.error(
                &format!("{app_path}/entryPageId"),
                "app entry page cannot require context parameters without an entry route",
            );
        }
        let mut nav_refs = BTreeSet::new();
        for (index, id) in app.navigation_item_ids.iter().enumerate() {
            if !nav_refs.insert(id.as_str()) {
                self.error(
                    &format!("{app_path}/navigationItemIds/{index}"),
                    "app navigation item ids must be unique",
                );
            }
            if !navigation_items.iter().any(|item| item.item_id == *id) {
                self.error(
                    &format!("{app_path}/navigationItemIds/{index}"),
                    "app references an unknown navigation item",
                );
            }
        }
        for (index, route) in app.routes.iter().enumerate() {
            let route_path = format!("{app_path}/routes/{index}");
            let Some(page) = self
                .pages
                .get(route.page_id.as_str())
                .map(|page| (*page).clone())
            else {
                self.error(
                    &format!("{route_path}/pageId"),
                    "route destination page does not exist",
                );
                continue;
            };
            if !app_pages.contains(route.page_id.as_str()) {
                self.error(
                    &format!("{route_path}/pageId"),
                    "route page must appear in app pageRefs",
                );
            }
            let captured = match route_parameters(&route.path) {
                Ok(parameters) => parameters,
                Err(message) => {
                    self.error(&format!("{route_path}/path"), message);
                    BTreeSet::new()
                }
            };
            let mut route_names = BTreeSet::new();
            let mut target_names = BTreeSet::new();
            for (param_index, mapping) in route.parameters.iter().enumerate() {
                if !route_names.insert(mapping.route_parameter.clone())
                    || !target_names.insert(mapping.context_parameter_id.clone())
                {
                    self.error(
                        &format!("{route_path}/parameters/{param_index}"),
                        "route parameter mappings must be unique",
                    );
                }
                if !captured.contains(&mapping.route_parameter) {
                    self.error(
                        &format!("{route_path}/parameters/{param_index}/routeParameter"),
                        "mapped route parameter is not captured by the route path",
                    );
                }
                let Some(param) = page
                    .context_params
                    .iter()
                    .find(|param| param.param_id == mapping.context_parameter_id)
                else {
                    self.error(
                        &format!("{route_path}/parameters/{param_index}/contextParameterId"),
                        "route parameter targets an undeclared page context parameter",
                    );
                    continue;
                };
                if !route_type_has_canonical_form(param.value_type) {
                    self.error(
                        &format!("{route_path}/parameters/{param_index}/contextParameterId"),
                        "route path cannot canonically decode this context parameter type",
                    );
                }
            }
            if captured != route_names {
                self.error(
                    &format!("{route_path}/parameters"),
                    "every captured route parameter must map exactly once to page context",
                );
            }
            for param in page.context_params.iter().filter(|param| param.required) {
                if !target_names.contains(param.param_id.as_str()) {
                    self.error(
                        &format!("{route_path}/parameters"),
                        format!(
                            "required page context parameter {:?} is not supplied by this route",
                            param.param_id
                        ),
                    );
                }
            }
        }
    }

    fn check_context_parameters(
        &mut self,
        target: &WidgetPage,
        values: &BTreeMap<String, WidgetFilterValue>,
        record_contexts: &BTreeSet<String>,
        path: &str,
    ) {
        for (id, value) in values {
            let Some(param) = target
                .context_params
                .iter()
                .find(|param| param.param_id == *id)
            else {
                self.error(
                    &format!("{path}/{id}"),
                    "navigation supplies an undeclared destination context parameter",
                );
                continue;
            };
            self.check_runtime_key_navigation_source(param, value, &format!("{path}/{id}"));
            let expected = value_type(param.value_type, param.required);
            let actual = self.filter_value_type(value, record_contexts, &format!("{path}/{id}"));
            if let Some(actual) = actual
                && !types_compatible(&actual, &expected)
            {
                self.error(
                    &format!("{path}/{id}"),
                    "navigation value does not match the destination context parameter type",
                );
            }
        }
        for param in target.context_params.iter().filter(|param| param.required) {
            if !values.contains_key(&param.param_id) {
                self.error(
                    path,
                    format!(
                        "required destination context parameter {:?} is not supplied",
                        param.param_id
                    ),
                );
            }
        }
    }

    fn check_runtime_key_navigation_source(
        &self,
        target: &norn_ir::widget_ux::WidgetContextParam,
        value: &WidgetFilterValue,
        path: &str,
    ) {
        match (&target.runtime_key_of, value) {
            (Some(expected), WidgetFilterValue::Record { property })
                if property.property_id == "$key" =>
            {
                if !self
                    .bindings
                    .get(property.binding_id.as_str())
                    .is_some_and(|binding| binding.object == *expected)
                {
                    self.error(
                        path,
                        "runtime-key navigation value must come from the exact marked entity binding",
                    );
                }
            }
            (
                Some(expected),
                WidgetFilterValue::ContextParameter {
                    page_id,
                    parameter_id,
                },
            ) => {
                if !self
                    .context_parameter(page_id, parameter_id)
                    .is_some_and(|source| {
                        source.required
                            && source.value_type == norn_ir::widget_ux::WidgetValueType::Text
                            && source.runtime_key_of.as_ref() == Some(expected)
                    })
                {
                    self.error(
                        path,
                        "runtime-key context parameter source must declare the same required text runtimeKeyOf entity",
                    );
                }
            }
            (Some(_), _) => self.error(
                path,
                "runtime-key context parameter requires a matching runtime-key binding or marked context parameter",
            ),
            (None, WidgetFilterValue::Record { property })
                if property.property_id == "$key" =>
            {
                self.error(
                    path,
                    "runtime-key navigation values require a matching runtimeKeyOf destination parameter",
                );
            }
            (
                None,
                WidgetFilterValue::ContextParameter {
                    page_id,
                    parameter_id,
                },
            ) if self
                .context_parameter(page_id, parameter_id)
                .is_some_and(|source| source.runtime_key_of.is_some()) => self.error(
                path,
                "runtime-key context parameters cannot be copied to unmarked destination parameters",
            ),
            _ => {}
        }
    }

    fn context_parameter(
        &self,
        page_id: &str,
        parameter_id: &str,
    ) -> Option<&norn_ir::widget_ux::WidgetContextParam> {
        self.pages.get(page_id).and_then(|page| {
            page.context_params
                .iter()
                .find(|param| param.param_id == parameter_id)
        })
    }

    fn navigation_record_contexts(&self, item_id: &str, path: &str) -> BTreeSet<String> {
        if self.pages_at(path).contains(&None) {
            return BTreeSet::new();
        }
        let mut contexts = BTreeSet::new();
        if let Some(uses) = self.navigation_uses.get(item_id)
            && let Some(Some(binding_id)) = uses.first()
            && uses
                .iter()
                .all(|context| context.as_deref() == Some(binding_id.as_str()))
        {
            contexts.insert(binding_id.clone());
        }
        for (binding_id, mode) in &self.list_selections {
            if *mode == WidgetSelectionMode::Single && self.list_rendered_at(binding_id, path) {
                contexts.insert(binding_id.clone());
            }
        }
        contexts
    }

    fn check_overrides(&mut self) {
        let overrides = self.ux.overrides.clone();
        let mut operations = BTreeSet::new();
        for (index, override_) in overrides.iter().enumerate() {
            let path = format!("/payload/ux/overrides/{index}");
            let operation = match &override_.value {
                WidgetOverrideValue::Label { .. } => "label",
                WidgetOverrideValue::Order { .. } => "order",
                WidgetOverrideValue::Grouping { .. } => "grouping",
                WidgetOverrideValue::Visibility { .. } => "visibility",
                WidgetOverrideValue::Control { .. } => "control",
                WidgetOverrideValue::ActionPlacement { .. } => "action_placement",
            };
            if !operations.insert((override_.target_semantic_id.as_str(), operation)) {
                self.error(
                    &format!("{path}/value"),
                    "a target semantic id may receive each override operation at most once",
                );
            }
            if !self
                .composed_semantic_ids
                .contains(&override_.target_semantic_id)
            {
                self.error(
                    &format!("{path}/targetSemanticId"),
                    "overrides must target a semantic id from a composed baseline page",
                );
            }
            let Some(target) = self
                .semantic_nodes
                .get(&override_.target_semantic_id)
                .cloned()
            else {
                self.error(
                    &format!("{path}/targetSemanticId"),
                    "override target does not exist in the composed component tree",
                );
                continue;
            };
            match &override_.value {
                WidgetOverrideValue::Label { value } => {
                    if value.0.is_empty() || value.0.values().any(|text| text.trim().is_empty()) {
                        self.error(
                            &format!("{path}/value"),
                            "label override must contain nonblank localized text",
                        );
                    }
                }
                WidgetOverrideValue::Order { .. } => {}
                WidgetOverrideValue::Grouping { value } => {
                    if value.trim().is_empty() {
                        self.error(
                            &format!("{path}/value"),
                            "grouping override must not be blank",
                        );
                    }
                }
                WidgetOverrideValue::Visibility { condition } => {
                    self.check_condition(condition, &format!("{path}/value/condition"));
                }
                WidgetOverrideValue::Control { control } => {
                    self.check_display_initialization(
                        *control,
                        &target.form_input_initialization,
                        &format!("{path}/value/control"),
                    );
                    let (Some(value_type), true) = (target.value_type, target.control_target)
                    else {
                        self.error(
                            &format!("{path}/value/control"),
                            "control override target must be a field semantic id",
                        );
                        continue;
                    };
                    self.check_control_type_with_option_type(
                        *control,
                        None,
                        target.option_key_type.as_ref(),
                        &value_type,
                        &path,
                    );
                }
                WidgetOverrideValue::ActionPlacement { placement_id } => {
                    if !target.action_target {
                        self.error(
                            &format!("{path}/value/placementId"),
                            "action placement override target must be an action semantic id",
                        );
                    }
                    if !self.placements.contains_key(placement_id.as_str()) {
                        self.error(
                            &format!("{path}/value/placementId"),
                            "action placement override names an unknown placement",
                        );
                    }
                }
            }
        }
    }

    fn check_optional_condition(&mut self, condition: Option<&ZenCondition>, path: &str) {
        if let Some(condition) = condition {
            self.check_condition(condition, path);
        }
    }

    fn check_condition(&mut self, condition: &ZenCondition, path: &str) {
        let Some(env) = self.expression_env(&condition.inputs, path) else {
            return;
        };
        let expression = match Expression::compile(condition.expression.as_str()) {
            Ok(expression) => expression,
            Err(error) => {
                self.error(path, format!("ZEN condition does not compile: {error}"));
                return;
            }
        };
        if let Err(errors) = check_expression(&expression, &env, &Type::Bool) {
            for error in errors {
                self.error(
                    path,
                    format!(
                        "ZEN condition must type-check to Boolean: {}",
                        error.message
                    ),
                );
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "This method assembles the closed, explicitly source-scoped ZEN input environment."
    )]
    fn expression_env(
        &mut self,
        inputs: &WidgetExpressionInputs,
        path: &str,
    ) -> Option<ZenTypeEnv> {
        let action_binding = path_index(path, "/payload/ux/actionBindings/").is_some();
        let mut valid = true;
        let mut records = BTreeMap::new();
        let mut input = BTreeMap::new();
        if let Some(grid_id) = &inputs.row_grid_id {
            let row_object = path_index(path, "/payload/ux/fields/")
                .and_then(|index| self.ux.fields.get(index))
                .and_then(|field| {
                    let WidgetFieldTarget::ChildRowProperty {
                        form_id,
                        collection_id,
                        ..
                    } = &field.target
                    else {
                        return None;
                    };
                    let grid = widget_node_by_id(self.ux, grid_id)?;
                    let matches_grid = matches!(
                        &grid.component,
                        WidgetComponent::ChildGrid {
                            form_id: grid_form,
                            collection_id: grid_collection,
                            field_ids,
                            ..
                        } if grid_form == form_id
                            && grid_collection == collection_id
                            && field_ids.contains(&field.field_id)
                    );
                    if !matches_grid {
                        return None;
                    }
                    let form = self.forms.get(form_id.as_str()).copied()?;
                    let action = self.action_for_form(form)?;
                    self.child_object(action, collection_id)
                });
            if let Some(row_object) = row_object {
                let name = self.unique_record_name();
                let row_fields = row_object
                    .properties
                    .iter()
                    .filter(|property| !property.computed)
                    .filter_map(|property| {
                        property_type(property)
                            .map(|type_| (property.id.as_str().to_owned(), type_))
                    })
                    .collect();
                records.insert(name.clone(), row_fields);
                input.insert("row".to_owned(), Type::Record(name));
            } else {
                self.error(
                    &format!("{path}/inputs/rowGridId"),
                    "input.row is available only to a child-row field condition naming its exact enclosing ChildGrid",
                );
                valid = false;
            }
        }
        if let Some(binding_id) = &inputs.record_binding_id {
            if !self.record_source_available_at(binding_id, path) {
                self.error(
                    &format!("{path}/inputs/recordBindingId"),
                    "record input requires a detail record, list row, or single-selection context",
                );
                valid = false;
            } else if let Some(record) =
                self.binding_record(binding_id, &format!("{path}/inputs/recordBindingId"))
            {
                let name = self.unique_record_name();
                records.insert(name.clone(), record);
                input.insert("record".to_owned(), Type::Record(name));
                input.insert("recordKey".to_owned(), Type::text());
                if action_binding {
                    input.insert("recordVersion".to_owned(), Type::text());
                }
            } else {
                valid = false;
            }
        }
        if let Some(form_id) = &inputs.draft_form_id {
            if !self.draft_source_available_at(form_id, path) {
                self.error(
                    &format!("{path}/inputs/draftFormId"),
                    if action_binding {
                        "input.draft requires a matching form opener, submit, or form footer at every invocation"
                    } else {
                        "input.draft must be available from the matching form at every use"
                    },
                );
                valid = false;
            } else if let Some(form) = self.forms.get(form_id.as_str()).copied().cloned() {
                if let Some(binding) = self
                    .action_bindings
                    .get(form.submit_binding_id.as_str())
                    .copied()
                    .cloned()
                {
                    if let Some(action) = self.actions.get(binding.binding_id.as_str()).cloned() {
                        let fields = action
                            .fields
                            .into_iter()
                            .filter(|(name, _)| form.input_names.contains(name))
                            .map(|(name, (schema, required))| {
                                let mut type_ = self.schema_type(&schema, &mut records);
                                if !required {
                                    type_ = type_.optional();
                                }
                                (name, type_)
                            })
                            .collect();
                        if form.input_names.iter().any(|name| {
                            !self
                                .actions
                                .get(binding.binding_id.as_str())
                                .is_some_and(|action| action.fields.contains_key(name))
                        }) {
                            self.error(
                                &format!("{path}/inputs/draftFormId"),
                                "draft form includes an input not declared by its ActionIR",
                            );
                            valid = false;
                        }
                        let name = self.unique_record_name();
                        records.insert(name.clone(), fields);
                        input.insert("draft".to_owned(), Type::Record(name));
                    } else {
                        self.error(
                            &format!("{path}/inputs/draftFormId"),
                            "draft form ActionIR schema is unavailable",
                        );
                        valid = false;
                    }
                } else {
                    self.error(
                        &format!("{path}/inputs/draftFormId"),
                        "draft form submit binding does not exist",
                    );
                    valid = false;
                }
            } else {
                self.error(
                    &format!("{path}/inputs/draftFormId"),
                    "draft input names an unknown form",
                );
                valid = false;
            }
        }
        if let Some(binding_id) = &inputs.selection_binding_id {
            if !self.selection_available_at(binding_id, path) {
                self.error(
                    &format!("{path}/inputs/selectionBindingId"),
                    if action_binding {
                        "input.selection must be available at every invocation; forms do not capture selection"
                    } else {
                        "input.selection must be available from the same list at every invocation; forms do not capture selection"
                    },
                );
                valid = false;
            } else if !self.is_bounded_list_binding(binding_id) {
                self.error(
                    &format!("{path}/inputs/selectionBindingId"),
                    "selection input must be a bounded list or traversal binding",
                );
                valid = false;
            } else if self
                .list_selections
                .get(binding_id)
                .is_none_or(|mode| *mode == WidgetSelectionMode::None)
            {
                self.error(
                    &format!("{path}/inputs/selectionBindingId"),
                    "selection input requires a list with single or multiple selection enabled",
                );
                valid = false;
            } else if let Some(record) =
                self.binding_record(binding_id, &format!("{path}/inputs/selectionBindingId"))
            {
                let name = self.unique_record_name();
                records.insert(name.clone(), record);
                input.insert("selection".to_owned(), Type::Record(name).list());
            } else {
                valid = false;
            }
        }
        if let Some(page_id) = &inputs.context_page_id {
            if !self.check_context_page_at(page_id, &format!("{path}/inputs/contextPageId")) {
                valid = false;
            }
            if let Some(page) = self.pages.get(page_id.as_str()).copied().cloned() {
                let name = self.unique_record_name();
                let fields = page
                    .context_params
                    .iter()
                    .map(|param| {
                        (
                            param.param_id.clone(),
                            value_type(param.value_type, param.required),
                        )
                    })
                    .collect();
                records.insert(name.clone(), fields);
                input.insert("context".to_owned(), Type::Record(name));
            } else {
                self.error(
                    &format!("{path}/inputs/contextPageId"),
                    "context input names an unknown page",
                );
                valid = false;
            }
        }
        match ZenTypeEnv::input_only_with_records(records, input, EnvOptions { max_link_depth: 0 })
        {
            Ok(env) if valid => Some(env),
            Ok(_) => None,
            Err(error) => {
                self.error(
                    path,
                    format!("could not construct explicit-input ZEN environment: {error}"),
                );
                None
            }
        }
    }

    fn binding_record(&mut self, binding_id: &str, path: &str) -> Option<BTreeMap<String, Type>> {
        let Some(binding) = self.bindings.get(binding_id).copied().cloned() else {
            self.error(path, "expression input names an unknown data binding");
            return None;
        };
        let entity = self.resolve_ref(
            &binding.object.id,
            &binding.object.version,
            &ArtifactKind::Entity,
            path,
        )?;
        let Some(Ok(object)) = self.catalog.object_type(&entity.artifact_id) else {
            self.error(path, "expression input object type is unavailable");
            return None;
        };
        Some(
            object
                .properties
                .iter()
                .map(|property| {
                    let name = property.id.as_str().to_owned();
                    let ty = property_type(property).unwrap_or(Type::Json);
                    (name, ty)
                })
                .collect(),
        )
    }

    fn schema_type(
        &mut self,
        schema: &Schema,
        records: &mut BTreeMap<String, BTreeMap<String, Type>>,
    ) -> Type {
        match schema {
            Schema::Object {
                properties,
                required,
            } => {
                let mut fields = BTreeMap::new();
                for (name, schema) in properties {
                    let mut ty = self.schema_type(schema, records);
                    if !required.contains(name) {
                        ty = ty.optional();
                    }
                    fields.insert(name.clone(), ty);
                }
                let name = self.unique_record_name();
                records.insert(name.clone(), fields);
                Type::Record(name)
            }
            Schema::Array(items) => self.schema_type(items, records).list(),
            Schema::String(None) => Type::text(),
            Schema::String(Some(values)) => Type::enumeration(values.iter().cloned()),
            Schema::Integer => Type::Integer,
            Schema::Number => Type::Number,
            Schema::Decimal(_) => Type::text(),
            Schema::Boolean => Type::Bool,
            Schema::Null | Schema::Json => Type::Json,
            Schema::Nullable(inner) => self.schema_type(inner, records).optional(),
        }
    }

    fn unique_record_name(&mut self) -> String {
        self.next_record = self.next_record.saturating_add(1);
        format!("norn:WidgetUx:{}", self.next_record)
    }

    fn filter_value_type(
        &mut self,
        value: &WidgetFilterValue,
        record_contexts: &BTreeSet<String>,
        path: &str,
    ) -> Option<Type> {
        match value {
            WidgetFilterValue::Literal { value } => Some(json_value_type(value)),
            WidgetFilterValue::ContextParameter {
                page_id,
                parameter_id,
            } => {
                self.check_context_page_at(page_id, path);
                let Some(page) = self.pages.get(page_id.as_str()) else {
                    self.error(path, "context value names an unknown page");
                    return None;
                };
                let Some(param) = page
                    .context_params
                    .iter()
                    .find(|param| param.param_id == *parameter_id)
                else {
                    self.error(path, "context value names an unknown page parameter");
                    return None;
                };
                Some(value_type(param.value_type, param.required))
            }
            WidgetFilterValue::Record { property } if property.property_id == "$key" => {
                if !record_contexts.contains(&property.binding_id) {
                    self.error(
                        path,
                        "runtime-key navigation value is not available in this list row, detail, or single-selection context",
                    );
                    return None;
                }
                if !self.bindings.contains_key(property.binding_id.as_str()) {
                    self.error(path, "runtime-key value names an unknown data binding");
                    return None;
                }
                Some(Type::text())
            }
            WidgetFilterValue::Record { property } => {
                if !record_contexts.contains(&property.binding_id) {
                    self.error(path, "record-derived navigation value is not available in this list row or detail context");
                    return None;
                }
                self.resolve_property(property, path)
                    .and_then(property_type)
            }
        }
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

    fn check_composition(
        &mut self,
        widget: &norn_ir::widget_ux::ArtifactRef,
        page_id: &str,
        host_page_id: &str,
        path: &str,
    ) {
        let Some(artifact) = self.resolve_ref(
            &widget.id,
            &widget.version,
            &ArtifactKind::Widget,
            &format!("{path}/widget"),
        ) else {
            return;
        };
        let identity = (
            widget.id.clone(),
            widget.version.clone(),
            page_id.to_owned(),
        );
        if self.composition_stack.contains(&identity) {
            self.error(path, "widget composition contains a recursive page cycle");
            return;
        }
        let ArtifactPayload::Widget(payload) = &artifact.payload else {
            return;
        };
        let Some(raw_ux) = payload.ux.as_value() else {
            self.error(path, "composed widget has no typed UX contract");
            return;
        };
        let Ok(ux) = serde_json::to_value(raw_ux).and_then(serde_json::from_value::<WidgetUx>)
        else {
            self.error(path, "composed widget root cannot be decoded");
            return;
        };
        let Some(selected_page) = ux.pages.iter().find(|page| page.page_id == page_id) else {
            self.error(
                &format!("{path}/pageId"),
                "composed widget page does not exist",
            );
            return;
        };
        if selected_page
            .context_params
            .iter()
            .any(|param| param.required)
        {
            self.error(
                &format!("{path}/pageId"),
                "composition cannot supply required context parameters to its baseline page",
            );
        }
        let mut selected_ids = BTreeSet::new();
        let mut selected_stack = BTreeSet::new();
        collect_composed_page_semantic_ids(
            self.all,
            artifact,
            &ux,
            page_id,
            &mut selected_stack,
            &mut selected_ids,
        );
        let mut nested_findings = Vec::new();
        let mut nested =
            UxChecker::new(artifact, self.all, self.catalog, &ux, &mut nested_findings);
        nested.enforce_effect_root = false;
        nested.composition_stack.clone_from(&self.composition_stack);
        nested.composition_stack.insert(identity);
        nested.check();
        self.import_rendered_lists(&nested, page_id, host_page_id, path);
        self.import_composed_semantics(&nested, &selected_ids, host_page_id, path);
        drop(nested);
        if !nested_findings.is_empty() {
            self.error(
                path,
                "composed widget page or one of its referenced pages fails typed UX validation",
            );
        }
    }

    fn import_composed_semantics(
        &mut self,
        source: &UxChecker<'_, '_, '_>,
        selected_ids: &BTreeSet<String>,
        host_page_id: &str,
        path: &str,
    ) {
        for (semantic_id, target) in source
            .semantic_nodes
            .iter()
            .filter(|(id, _)| selected_ids.contains(*id))
        {
            if self.local_semantic_ids.contains(semantic_id)
                || self.local_field_ids.contains(semantic_id)
                || self.semantic_nodes.contains_key(semantic_id)
            {
                self.error(
                    path,
                    format!(
                        "composed semantic id {semantic_id:?} collides with another component id"
                    ),
                );
                continue;
            }
            self.composed_semantic_ids.insert(semantic_id.clone());
            if source.form_owned_semantics.contains(semantic_id) {
                self.form_owned_semantics.insert(semantic_id.clone());
            }
            self.page_uses
                .entry(Usage::Semantic(semantic_id.clone()))
                .or_default()
                .insert(Some(host_page_id.to_owned()));
            if let Some(surface) = source.action_surfaces.get(semantic_id) {
                self.action_surfaces
                    .insert(semantic_id.clone(), surface.clone());
            }
            if let Some(surfaces) = source
                .usage_surfaces
                .get(&Usage::Semantic(semantic_id.clone()))
            {
                for surface in surfaces {
                    let surface = match surface {
                        ActionSurface::List(id) | ActionSurface::Detail(id)
                            if source.bindings.get(id.as_str()).copied()
                                != self.bindings.get(id.as_str()).copied() =>
                        {
                            ActionSurface::Page
                        }
                        ActionSurface::Form(id)
                            if source.forms.get(id.as_str()).copied()
                                != self.forms.get(id.as_str()).copied() =>
                        {
                            ActionSurface::Page
                        }
                        ActionSurface::Page
                        | ActionSurface::List(_)
                        | ActionSurface::Detail(_)
                        | ActionSurface::Form(_) => surface.clone(),
                    };
                    self.add_surface_use(Usage::Semantic(semantic_id.clone()), &surface);
                }
            }
            self.semantic_nodes
                .insert(semantic_id.clone(), target.clone());
        }
    }

    fn import_rendered_lists(
        &mut self,
        source: &UxChecker<'_, '_, '_>,
        page_id: &str,
        host_page_id: &str,
        path: &str,
    ) {
        for (id, pages) in &source.rendered_lists {
            if !pages.contains(&Some(page_id.to_owned())) {
                continue;
            }
            let Some(binding) = source.bindings.get(id.as_str()).copied() else {
                continue;
            };
            if self.bindings.get(id.as_str()).copied() != Some(binding) {
                continue;
            }
            let Some(selection) = source.list_selections.get(id).copied() else {
                continue;
            };
            if self
                .list_selections
                .get(id)
                .is_some_and(|mode| *mode != selection)
            {
                self.error(
                    path,
                    "composed list cannot use a conflicting host selection mode",
                );
                continue;
            }
            self.list_selections.insert(id.clone(), selection);
            self.list_row_contexts.insert(id.clone());
            self.rendered_lists
                .entry(id.clone())
                .or_default()
                .insert(Some(host_page_id.to_owned()));
        }
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

fn add_input_uses(
    edges: &mut Vec<(Usage, Usage)>,
    source: &Usage,
    inputs: &WidgetExpressionInputs,
) {
    for id in [&inputs.record_binding_id, &inputs.selection_binding_id]
        .into_iter()
        .flatten()
    {
        edges.push((source.clone(), Usage::Binding(id.clone())));
    }
    if let Some(id) = &inputs.draft_form_id {
        edges.push((source.clone(), Usage::Form(id.clone())));
    }
}

fn direct_draft_mapping(binding: &WidgetUxActionBinding, input_name: &str) -> bool {
    let expected = format!("input.draft.{input_name}");
    binding
        .input_mapping
        .get(input_name)
        .is_some_and(|expression| expression.as_str().trim() == expected)
}

fn child_grid_component<'a>(
    ux: &'a WidgetUx,
    form_id: &str,
    collection_id: &str,
) -> Option<&'a WidgetComponent> {
    fn find<'a>(
        node: &'a WidgetNode,
        form_id: &str,
        collection_id: &str,
    ) -> Option<&'a WidgetComponent> {
        if matches!(
            &node.component,
            WidgetComponent::ChildGrid {
                form_id: owner,
                collection_id: collection,
                ..
            } if owner == form_id && collection == collection_id
        ) {
            return Some(&node.component);
        }
        node.children
            .iter()
            .find_map(|child| find(child, form_id, collection_id))
    }

    ux.pages
        .iter()
        .find_map(|page| find(&page.root, form_id, collection_id))
}

fn widget_node_by_id<'a>(ux: &'a WidgetUx, node_id: &str) -> Option<&'a WidgetNode> {
    fn find<'a>(node: &'a WidgetNode, node_id: &str) -> Option<&'a WidgetNode> {
        if node.node_id == node_id {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, node_id))
    }

    ux.pages.iter().find_map(|page| find(&page.root, node_id))
}

fn add_value_use(edges: &mut Vec<(Usage, Usage)>, source: &Usage, value: &WidgetFilterValue) {
    match value {
        WidgetFilterValue::Record { property } => {
            edges.push((source.clone(), Usage::Binding(property.binding_id.clone())));
        }
        WidgetFilterValue::Literal { .. } | WidgetFilterValue::ContextParameter { .. } => {}
    }
}

fn binding_record_dependencies(binding: &WidgetDataBinding, index: usize) -> Vec<(&str, String)> {
    let path = format!("/payload/ux/dataBindings/{index}/query");
    match &binding.query {
        WidgetDataQuery::ObjectList { filters, .. } => filters
            .iter()
            .enumerate()
            .filter_map(|(index, filter)| {
                if let WidgetFilterValue::Record { property } = &filter.value {
                    Some((
                        property.binding_id.as_str(),
                        format!("{path}/filters/{index}/value/property/bindingId"),
                    ))
                } else {
                    None
                }
            })
            .collect(),
        WidgetDataQuery::ObjectRuntimeDetail { .. } => Vec::new(),
        WidgetDataQuery::ObjectDetail { key } => key
            .iter()
            .enumerate()
            .filter_map(|(index, member)| {
                if let WidgetFilterValue::Record { property } = &member.value {
                    Some((
                        property.binding_id.as_str(),
                        format!("{path}/key/{index}/value/property/bindingId"),
                    ))
                } else {
                    None
                }
            })
            .collect(),
        WidgetDataQuery::LinkTraversal {
            source_binding_id, ..
        } => vec![(
            source_binding_id.as_str(),
            format!("{path}/sourceBindingId"),
        )],
    }
}

fn finding(owner: &ArtifactEnvelope, path: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(CODE),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        span: Some(Span::pointer(path)),
        artifact_id: Some(owner.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
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

fn property_type(property: &EffectiveProperty) -> Option<Type> {
    let ty = match &property.kind {
        EffectiveKind::Base(PropertyType::Text { format }) => Type::Text { format: *format },
        EffectiveKind::Base(PropertyType::Integer {}) => Type::Integer,
        EffectiveKind::Base(PropertyType::Decimal { scale, unit, .. }) => Type::Decimal {
            scale: scale.0,
            unit: unit.clone(),
        },
        EffectiveKind::Base(PropertyType::Boolean {}) => Type::Bool,
        EffectiveKind::Base(PropertyType::Timestamp { timezone }) => Type::Timestamp {
            timezone: *timezone,
        },
        EffectiveKind::Base(PropertyType::Date {}) => Type::Date,
        EffectiveKind::Base(PropertyType::Duration {}) => Type::Duration,
        EffectiveKind::Base(PropertyType::Enum { values }) => {
            Type::enumeration(values.iter().map(|value| value.value.clone()))
        }
        EffectiveKind::Base(
            PropertyType::Record { .. } | PropertyType::JsonExt { .. } | PropertyType::Ref { .. },
        )
        | EffectiveKind::Registry(_) => Type::Json,
        EffectiveKind::Unresolved => return None,
    };
    Some(if property.nullable { ty.optional() } else { ty })
}

fn value_type(value_type: norn_ir::WidgetValueType, required: bool) -> Type {
    let ty = match value_type {
        norn_ir::WidgetValueType::Text => Type::text(),
        norn_ir::WidgetValueType::Integer => Type::Integer,
        norn_ir::WidgetValueType::Number => Type::Number,
        norn_ir::WidgetValueType::Boolean => Type::Bool,
        norn_ir::WidgetValueType::Date => Type::Date,
        norn_ir::WidgetValueType::Timestamp => Type::Timestamp {
            timezone: norn_ir::TimezonePolicy::Utc,
        },
        norn_ir::WidgetValueType::Duration => Type::Duration,
        norn_ir::WidgetValueType::Json => Type::Json,
    };
    if required { ty } else { ty.optional() }
}

fn types_compatible(source: &Type, expected: &Type) -> bool {
    if source == expected {
        return true;
    }
    match (source, expected) {
        (Type::Optional(source), Type::Optional(expected)) => types_compatible(source, expected),
        (source, Type::Optional(expected)) => types_compatible(source, expected),
        (Type::Integer | Type::Decimal { .. }, Type::Number)
        | (Type::Enum { .. }, Type::Text { .. }) => true,
        _ => false,
    }
}

fn is_ordered_filter_operator(operator: WidgetFilterOperator) -> bool {
    matches!(
        operator,
        WidgetFilterOperator::Lt
            | WidgetFilterOperator::Lte
            | WidgetFilterOperator::Gt
            | WidgetFilterOperator::Gte
    )
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
fn is_orderable_type(ty: &Type) -> bool {
    match ty {
        Type::Optional(inner) => is_orderable_type(inner),
        Type::Text { .. }
        | Type::Enum { .. }
        | Type::Integer
        | Type::Number
        | Type::Decimal { .. }
        | Type::Date
        | Type::Timestamp { .. } => true,
        _ => false,
    }
}

fn type_fits_schema(source: &Type, schema: &Schema) -> bool {
    match (source, schema) {
        (Type::Optional(source), Schema::Nullable(expected)) => type_fits_schema(source, expected),
        (Type::Optional(_), _) | (Type::Json, Schema::Null) => false,
        (_, Schema::Nullable(expected)) => type_fits_schema(source, expected),
        (Type::Integer, Schema::Integer | Schema::Number)
        | (Type::Number | Type::Decimal { .. }, Schema::Number)
        | (Type::Bool, Schema::Boolean)
        | (
            Type::Text { .. } | Type::Enum { .. } | Type::Date | Type::Timestamp { .. },
            Schema::String(_),
        )
        | (Type::Json, Schema::Json) => true,
        (Type::List(item), Schema::Array(expected)) => type_fits_schema(item, expected),
        _ => false,
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
fn type_is_integer_source(type_: &Type) -> bool {
    match type_ {
        Type::Integer => true,
        Type::Optional(inner) => type_is_integer_source(inner),
        _ => false,
    }
}

fn schema_accepts_empty_array(schema: &Schema) -> bool {
    match schema {
        Schema::Array(_) | Schema::Json => true,
        Schema::Nullable(inner) => schema_accepts_empty_array(inner),
        Schema::Object { .. }
        | Schema::String(_)
        | Schema::Integer
        | Schema::Number
        | Schema::Decimal(_)
        | Schema::Boolean
        | Schema::Null => false,
    }
}

fn json_value_type(value: &JsonValue) -> Type {
    match value {
        JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => Type::Json,
        JsonValue::Bool(_) => Type::Bool,
        JsonValue::Number(number) if number.fract() == 0.0 => Type::Integer,
        JsonValue::Number(_) => Type::Number,
        JsonValue::String(_) => Type::text(),
    }
}

fn inferred_matches_schema(
    inferred: &InferredType,
    schema: &Schema,
    sources: Option<&[&ConstructionSource]>,
    integer_proof: &mut impl FnMut(&[&ConstructionSource], &str) -> bool,
    path: &str,
) -> Result<(), String> {
    match (inferred, schema) {
        (InferredType::Optional(actual), Schema::Nullable(expected)) => {
            let nonnull: Option<Vec<_>> = sources.map(|sources| {
                sources
                    .iter()
                    .copied()
                    .filter(|source| {
                        !matches!(source, ConstructionSource::Scalar(source) if source.trim() == "null")
                    })
                    .collect()
            });
            inferred_matches_schema(actual, expected, nonnull.as_deref(), integer_proof, path)
        }
        (InferredType::Optional(_), _) => Err(format!(
            "{path} may be null but the ActionIR schema is non-nullable"
        )),
        (actual, Schema::Nullable(expected)) => {
            if matches!(actual, InferredType::Null) {
                Ok(())
            } else {
                inferred_matches_schema(actual, expected, sources, integer_proof, path)
            }
        }
        (
            InferredType::Record(actual),
            Schema::Object {
                properties,
                required,
            },
        ) => inferred_record_matches_schema(
            actual,
            properties,
            required,
            sources,
            integer_proof,
            path,
        ),
        (InferredType::List(actual), Schema::Array(expected)) => {
            let items = sources.and_then(list_item_sources);
            inferred_matches_schema(
                actual,
                expected,
                items.as_deref(),
                integer_proof,
                &format!("{path}/*"),
            )
        }
        (InferredType::Const(value), Schema::String(Some(allowed))) if allowed.contains(value) => {
            Ok(())
        }
        (InferredType::Enum(values), Schema::String(Some(allowed)))
            if values.iter().all(|value| allowed.contains(value)) =>
        {
            Ok(())
        }
        (InferredType::String, Schema::String(Some(_))) => Err(format!(
            "{path} is unconstrained text but ActionIR requires one of the declared string values"
        )),
        (InferredType::Const(value), Schema::String(Some(allowed))) => Err(format!(
            "{path} produces undeclared string value {value:?}; allowed values are {allowed:?}"
        )),
        (InferredType::Enum(values), Schema::String(Some(allowed))) => Err(format!(
            "{path} produces string values {values:?} outside ActionIR values {allowed:?}"
        )),
        (InferredType::String, Schema::Decimal(_)) => Ok(()),
        (InferredType::Const(text), decimal @ Schema::Decimal(_)) => decimal
            .decode_action_input(norn_ir::RuntimeValue::String(text.clone()), path)
            .map(|_value| ())
            .map_err(|error| error.reason),
        (InferredType::Enum(values), decimal @ Schema::Decimal(_)) => {
            for text in values {
                decimal
                    .decode_action_input(norn_ir::RuntimeValue::String(text.clone()), path)
                    .map_err(|error| error.reason)?;
            }
            Ok(())
        }
        (InferredType::Null, Schema::Null)
        | (
            InferredType::String
            | InferredType::Const(_)
            | InferredType::Enum(_)
            | InferredType::Date
            | InferredType::Timestamp,
            Schema::String(None),
        )
        | (InferredType::Bool, Schema::Boolean)
        | (_, Schema::Json) => Ok(()),
        (InferredType::Number, Schema::Integer | Schema::Number)
            if matches!(schema, Schema::Number)
                || sources.is_some_and(|sources| integer_proof(sources, path)) =>
        {
            Ok(())
        }
        (InferredType::Number, Schema::Integer) => Err(format!(
            "{path} has no proof of an integer value; use a direct Integer input or exact integer literal"
        )),
        (actual, expected) => Err(format!(
            "{path} produces {actual}, incompatible with ActionIR schema {expected:?}"
        )),
    }
}

fn inferred_record_matches_schema(
    actual: &BTreeMap<String, InferredType>,
    properties: &BTreeMap<String, Schema>,
    required: &BTreeSet<String>,
    sources: Option<&[&ConstructionSource]>,
    integer_proof: &mut impl FnMut(&[&ConstructionSource], &str) -> bool,
    path: &str,
) -> Result<(), String> {
    if let Some(extra) = actual.keys().find(|name| !properties.contains_key(*name)) {
        return Err(format!(
            "{path} constructs undeclared ActionIR object member {extra:?}"
        ));
    }
    if let Some(missing) = required.iter().find(|name| !actual.contains_key(*name)) {
        return Err(format!(
            "{path} omits required ActionIR object member {missing:?}"
        ));
    }
    for (name, value) in actual {
        if let Some(expected) = properties.get(name) {
            let members = sources.and_then(|sources| record_member_sources(sources, name));
            inferred_matches_schema(
                value,
                expected,
                members.as_deref(),
                integer_proof,
                &format!("{path}/{name}"),
            )?;
        }
    }
    Ok(())
}

fn record_member_sources<'s>(
    sources: &[&'s ConstructionSource],
    name: &str,
) -> Option<Vec<&'s ConstructionSource>> {
    sources
        .iter()
        .map(|source| match source {
            ConstructionSource::Record(fields) => fields.get(name),
            ConstructionSource::Scalar(_) | ConstructionSource::List(_) => None,
        })
        .collect()
}

fn list_item_sources<'s>(
    sources: &[&'s ConstructionSource],
) -> Option<Vec<&'s ConstructionSource>> {
    let mut items = Vec::new();
    for source in sources {
        let ConstructionSource::List(children) = source else {
            return None;
        };
        items.extend(children);
    }
    Some(items)
}

fn route_parameters(path: &str) -> Result<BTreeSet<String>, &'static str> {
    if !path.starts_with('/') || path.contains('?') || path.contains('#') {
        return Err("route path must be absolute and cannot contain a query or fragment");
    }
    if path == "/" {
        return Ok(BTreeSet::new());
    }
    let segments = path
        .strip_prefix('/')
        .ok_or("route path must be absolute")?
        .split('/');
    let mut parameters = BTreeSet::new();
    for segment in segments {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("route path cannot contain empty or dot segments");
        }
        if let Some(name) = segment
            .strip_prefix('{')
            .and_then(|segment| segment.strip_suffix('}'))
        {
            let mut chars = name.chars();
            let Some(first) = chars.next() else {
                return Err("route parameter names cannot be blank");
            };
            if !(first.is_ascii_alphabetic() || first == '_')
                || !chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                return Err("route parameters must use complete {identifier} path segments");
            }
            if !parameters.insert(name.to_owned()) {
                return Err("route parameter names must be unique in a path");
            }
        } else if segment.contains('{')
            || segment.contains('}')
            || segment.contains('*')
            || !segment.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '~')
            })
        {
            return Err(
                "route path segments must be literal safe names or complete {identifier} parameters",
            );
        }
    }
    Ok(parameters)
}

fn route_type_has_canonical_form(value_type: norn_ir::WidgetValueType) -> bool {
    matches!(
        value_type,
        norn_ir::WidgetValueType::Text
            | norn_ir::WidgetValueType::Integer
            | norn_ir::WidgetValueType::Number
            | norn_ir::WidgetValueType::Boolean
            | norn_ir::WidgetValueType::Date
            | norn_ir::WidgetValueType::Timestamp
            | norn_ir::WidgetValueType::Duration
    )
}

fn collect_composed_page_semantic_ids(
    all: &[ArtifactEnvelope],
    artifact: &ArtifactEnvelope,
    ux: &WidgetUx,
    page_id: &str,
    stack: &mut BTreeSet<(String, String, String)>,
    out: &mut BTreeSet<String>,
) {
    let identity = (
        artifact.artifact_id.clone(),
        artifact.version.clone(),
        page_id.to_owned(),
    );
    if !stack.insert(identity.clone()) {
        return;
    }
    if let Some(page) = ux.pages.iter().find(|page| page.page_id == page_id) {
        collect_tree_semantic_ids(all, ux, &page.root, stack, out);
    }
    stack.remove(&identity);
}

fn collect_tree_semantic_ids(
    all: &[ArtifactEnvelope],
    ux: &WidgetUx,
    node: &WidgetNode,
    stack: &mut BTreeSet<(String, String, String)>,
    out: &mut BTreeSet<String>,
) {
    if let Some(id) = &node.semantic_id {
        out.insert(id.clone());
    }
    match &node.component {
        WidgetComponent::Section { section } => {
            for property in &section.fields {
                for field in &ux.fields {
                    if matches!(&field.target, WidgetFieldTarget::Property { property: target }
                        if target.binding_id == property.binding_id && target.property_id == property.property_id)
                    {
                        out.insert(field.field_id.clone());
                    }
                }
            }
        }
        WidgetComponent::List { fields, .. } => {
            for property in fields {
                for field in &ux.fields {
                    if matches!(&field.target, WidgetFieldTarget::Property { property: target }
                        if target.binding_id == property.binding_id && target.property_id == property.property_id)
                    {
                        out.insert(field.field_id.clone());
                    }
                }
            }
        }
        WidgetComponent::ChildGrid { field_ids, .. } => {
            out.extend(field_ids.iter().cloned());
        }
        WidgetComponent::Field { field_id } => {
            out.insert(field_id.clone());
        }
        WidgetComponent::Composition { widget, page_id } => {
            if let Some(composed) = all.iter().find(|candidate| {
                candidate.kind() == ArtifactKind::Widget
                    && candidate.artifact_id == widget.id
                    && candidate.version == widget.version
            }) && let ArtifactPayload::Widget(payload) = &composed.payload
                && let Some(raw_ux) = payload.ux.as_value()
                && let Ok(composed_ux) =
                    serde_json::to_value(raw_ux).and_then(serde_json::from_value::<WidgetUx>)
            {
                collect_composed_page_semantic_ids(
                    all,
                    composed,
                    &composed_ux,
                    page_id,
                    stack,
                    out,
                );
            }
        }
        WidgetComponent::Form { .. }
        | WidgetComponent::Action { .. }
        | WidgetComponent::Navigation { .. }
        | WidgetComponent::Detail { .. }
        | WidgetComponent::Dialog { .. } => {}
    }
    for child in &node.children {
        collect_tree_semantic_ids(all, ux, child, stack, out);
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

#[cfg(test)]
mod decimal_wire_tests {
    use super::*;

    #[test]
    fn action_mapping_accepts_decimal_text_but_not_numeric_wire_values() {
        let accepts = |inferred| {
            inferred_matches_schema(
                &inferred,
                &Schema::Decimal(2),
                None,
                &mut |_, _| false,
                "amount",
            )
        };
        assert!(accepts(InferredType::String).is_ok());
        assert!(accepts(InferredType::Const("125.50".to_owned())).is_ok());
        assert!(accepts(InferredType::Number).is_err());
        assert!(accepts(InferredType::Const("1.001".to_owned())).is_err());
    }
}
