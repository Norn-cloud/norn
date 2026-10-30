//! Interaction rules over a server-driven UI document (#3372, #3319 "Verified UI").
//!
//! A widget's typed UX contract is lowered into base facts (relations over node, page,
//! item and binding ids). Stratified rules then derive violations. The rules are plain
//! relational joins with negation over finite sets, written in the Datalog notation of
//! each `// rule:` comment so they can move to `norn-datalog` unchanged; Rust owns only
//! fact extraction (`Facts::extract`) and diagnostics. Server and permission checks stay
//! server-side: these rules read the document only and add no client-side authority.
//!
//! Strata (a relation may only be negated or aggregated from a strictly lower stratum; negating a
//! base fact such as `!hidden_item` or `!app_page` does not raise a stratum):
//! - 0, positive over base facts: `ancestor` and `reach` (recursive), `control_edge`, `seed`,
//!   `bad_label`, `has_dismissal`, `keyboard_close`, `has_invoker`, `restore_ok`, `rendered_item`,
//!   `used_binding`, `illegal_control`, `no_clear`.
//! - 1, negation over stratum 0: `named`, `no_keyboard_close`, `no_restore`, `unreachable_page`,
//!   `missing_control`, `dead_binding`, `kb_trap`; and aggregation (needs `count` in the target
//!   language): `too_deep`.
//! - 2, negation over stratum 1: `unnamed` (negates `named`).
//!
//! What these rules establish is **structural**: a restore target, a close path or a
//! control exists *in the document*. They do not prove that the target is still mounted
//! when a dialog closes (the renderer statechart in PR #3598 handles that at run time) and
//! they do not see controls that live in other artifacts, except composed baseline pages: the
//! selected page of a `Composition` node is walked in the host's context, with the host's label
//! overrides applied, so a name an override empties is found. Navigation, bindings and nullable
//! inputs of a composed page belong to the baseline artifact and stay checked there.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, LocaleText, WidgetArtifactRef, WidgetComponent, WidgetDismissalMethod,
    WidgetFieldControl, WidgetFieldTarget, WidgetNavigationDestination, WidgetNode,
    WidgetOverrideValue, WidgetSectionKind, WidgetUx,
};

use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

/// Deepest dialog nesting the renderer interaction statechart models (`NDLG` in
/// `knowledge/verification/sdui-interaction-v1/models/interaction.qnt`, PR #3598).
pub const VERIFIED_MODAL_DEPTH: usize = 3;

const NULLABLE_WITHOUT_CLEAR: &str = "widget.interaction.nullable_without_clear";
const UNNAMED_CONTROL: &str = "widget.interaction.unnamed_control";
const DIALOG_NO_KEYBOARD_CLOSE: &str = "widget.interaction.dialog_no_keyboard_close";
const DIALOG_NO_RESTORE_TARGET: &str = "widget.interaction.dialog_no_restore_target";
const DIALOG_DEPTH: &str = "widget.interaction.dialog_depth_exceeds_verified_bound";
const UNREACHABLE_PAGE: &str = "widget.interaction.unreachable_page";
const CONTROL_MISSING: &str = "widget.interaction.control_missing_for_transition";
const CONTROL_ILLEGAL: &str = "widget.interaction.control_without_legal_transition";
const KEYBOARD_UNREACHABLE: &str = "widget.interaction.keyboard_unreachable_control";

type Pair = (String, String);

/// Resolves a pinned widget reference to its typed UX contract (composed baselines).
pub(crate) type ComposedUx<'r> = dyn Fn(&WidgetArtifactRef) -> Option<WidgetUx> + 'r;

/// Base facts extracted from one UX document. Every field is a relation; the
/// pointer maps only locate findings and take no part in the rules.
#[derive(Debug, Default)]
struct Facts {
    app_present: bool,
    // entry(P). routed_page(P). app_page(P). page_named(P).
    entry: BTreeSet<String>,
    routed_page: BTreeSet<String>,
    app_page: BTreeSet<String>,
    page_named: BTreeSet<String>,
    // named_scope(E): E sits on a named composed baseline page (its own page name counts as a region).
    named_scope: BTreeSet<String>,
    // in_page(N, P). parent(N, Parent).
    in_page: BTreeMap<String, String>,
    parent: BTreeMap<String, String>,
    // interactive(N). label(N, Locale, Ok). has_label(N).
    interactive: BTreeSet<String>,
    label: BTreeSet<(String, String, bool)>,
    has_label: BTreeSet<String>,
    // dialog(N). dismissal(N, Method). action_node(N). named_container(N).
    dialog: BTreeSet<String>,
    dismissal: BTreeSet<Pair>,
    action_node: BTreeSet<String>,
    named_container: BTreeSet<String>,
    // nullable_field(F). clear_capable(F). always_required(F).
    nullable_field: BTreeSet<String>,
    clear_capable: BTreeSet<String>,
    always_required: BTreeSet<String>,
    // declared_item(I). hidden_item(I). shell_item(I). nav_node(N, I). item_page(I, Q).
    declared_item: BTreeSet<String>,
    hidden_item: BTreeSet<String>,
    shell_item: BTreeSet<String>,
    nav_node: BTreeSet<Pair>,
    item_page: BTreeMap<String, String>,
    // action_binding(B). uses(B).
    action_binding: BTreeSet<String>,
    uses: BTreeSet<String>,
    node_pointer: BTreeMap<String, String>,
    page_pointer: BTreeMap<String, String>,
    item_pointer: BTreeMap<String, String>,
    binding_pointer: BTreeMap<String, String>,
}

/// One derived violation: the registered code, the pointer, and the message.
struct Finding {
    code: &'static str,
    pointer: String,
    message: String,
}

/// Check the interaction rules of one widget's typed UX contract. `nullable_inputs` holds the
/// `(form id, input name)` pairs whose submit `ActionIR` leaf the server declares nullable;
/// `composed` resolves the baselines a `Composition` node pins.
#[must_use]
pub(crate) fn check(
    owner: &ArtifactEnvelope,
    ux: &WidgetUx,
    nullable_inputs: &BTreeSet<Pair>,
    composed: &ComposedUx<'_>,
) -> Vec<Diagnostic> {
    derive(&Facts::extract(ux, nullable_inputs, composed))
        .into_iter()
        .map(|finding| Diagnostic {
            code: DiagnosticCode::new(finding.code),
            severity: DiagnosticSeverity::Error,
            message: finding.message,
            span: Some(Span::pointer(finding.pointer)),
            artifact_id: Some(owner.artifact_id.clone()),
            remediation: Some(DiagnosticRemediation::FixDefinition),
        })
        .collect()
}

// Element keys are injective across kinds, so a generated element can never collide with a declared
// node id: `n:` declared node, `s:` shell item, `i:` grid row input, `c:` node of a composed baseline.
fn node_key(id: &str) -> String {
    format!("n:{id}")
}

fn shell_key(item: &str) -> String {
    format!("s:{item}")
}

fn input_key(grid: &str, field: &str) -> String {
    format!("i:{}:{grid}{field}", grid.len())
}

fn composed_key(host: &str, id: &str) -> String {
    format!("c:{}:{host}{id}", host.len())
}

/// The author-facing name of an element key, for messages.
fn show(key: &str) -> String {
    fn split(rest: &str) -> Option<(&str, &str)> {
        let (len, tail) = rest.split_once(':')?;
        let len: usize = len.parse().ok()?;
        (tail.is_char_boundary(len)).then(|| tail.split_at(len))
    }
    match key.split_once(':') {
        Some(("n", id)) => id.to_owned(),
        Some(("s", item)) => format!("shell item {item}"),
        Some(("i", rest)) => split(rest).map_or_else(
            || key.to_owned(),
            |(grid, field)| format!("{}#{field}", show(grid)),
        ),
        Some(("c", rest)) => split(rest).map_or_else(
            || key.to_owned(),
            |(host, id)| format!("{}::{id}", show(host)),
        ),
        _ => key.to_owned(),
    }
}

fn label_overrides(ux: &WidgetUx) -> BTreeMap<&str, &LocaleText> {
    ux.overrides
        .iter()
        .filter_map(|over| match &over.value {
            WidgetOverrideValue::Label { value } => Some((over.target_semantic_id.as_str(), value)),
            WidgetOverrideValue::Order { .. }
            | WidgetOverrideValue::Grouping { .. }
            | WidgetOverrideValue::Visibility { .. }
            | WidgetOverrideValue::Control { .. }
            | WidgetOverrideValue::ActionPlacement { .. } => None,
        })
        .collect()
}

fn text_is_named(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

fn locale_text_is_named(text: &LocaleText) -> bool {
    !text.0.is_empty() && text.0.values().all(|value| text_is_named(value))
}

/// Controls the renderer can clear to an explicit null: free-form and date inputs and checkboxes
/// through the Clear control (PR #3600), closed choices through their "not set" option. A
/// read-only `display` has no input to clear, so a new control kind must be classified here.
const fn clear_capable(control: WidgetFieldControl) -> bool {
    match control {
        WidgetFieldControl::Text
        | WidgetFieldControl::TextArea
        | WidgetFieldControl::Number
        | WidgetFieldControl::Date
        | WidgetFieldControl::DateTime
        | WidgetFieldControl::Checkbox
        | WidgetFieldControl::Select
        | WidgetFieldControl::Link => true,
        WidgetFieldControl::Display => false,
    }
}

fn dismissal_name(method: WidgetDismissalMethod) -> &'static str {
    match method {
        WidgetDismissalMethod::VisibleClose => "visible_close",
        WidgetDismissalMethod::Scrim => "scrim",
        WidgetDismissalMethod::Swipe => "swipe",
        WidgetDismissalMethod::Escape => "escape",
    }
}

/// A condition the author wrote as the literal `false` hides its control permanently.
fn never_visible(condition: Option<&norn_ir::ZenCondition>) -> bool {
    condition.is_some_and(|condition| condition.expression.as_str().trim() == "false")
}

struct Context<'a> {
    placements: &'a BTreeMap<&'a str, &'a norn_ir::WidgetActionPlacement>,
    items: &'a BTreeMap<&'a str, &'a norn_ir::WidgetNavigationItem>,
    fields: &'a BTreeMap<&'a str, &'a norn_ir::WidgetFieldPresentation>,
    forms: &'a BTreeMap<&'a str, &'a norn_ir::WidgetForm>,
    nullable: &'a BTreeSet<Pair>,
    /// Label overrides by semantic id: the effective label of a node is the override when present.
    label_overrides: BTreeMap<&'a str, &'a LocaleText>,
    composed: &'a ComposedUx<'a>,
    /// Composed baselines being walked, to stop a cycle.
    stack: Vec<(String, String, String)>,
    /// Set while walking a composed baseline page: ids are prefixed with the host node, pointers stay
    /// at the host `Composition` node, and the baseline's own navigation, bindings and nullable inputs
    /// are left to the baseline's artifact.
    host: Option<String>,
}

impl Context<'_> {
    fn effective<'t>(&'t self, node: &WidgetNode, declared: &'t LocaleText) -> &'t LocaleText {
        node.semantic_id
            .as_deref()
            .and_then(|semantic| self.label_overrides.get(semantic).copied())
            .unwrap_or(declared)
    }
}

impl Facts {
    fn extract(ux: &WidgetUx, nullable_inputs: &BTreeSet<Pair>, composed: &ComposedUx<'_>) -> Self {
        let mut facts = Self::default();
        let placements: BTreeMap<&str, &norn_ir::WidgetActionPlacement> = ux
            .action_placements
            .iter()
            .map(|placement| (placement.placement_id.as_str(), placement))
            .collect();
        let items: BTreeMap<&str, &norn_ir::WidgetNavigationItem> = ux
            .navigation_items
            .iter()
            .map(|item| (item.item_id.as_str(), item))
            .collect();
        let fields: BTreeMap<&str, &norn_ir::WidgetFieldPresentation> = ux
            .fields
            .iter()
            .map(|field| (field.field_id.as_str(), field))
            .collect();
        let forms: BTreeMap<&str, &norn_ir::WidgetForm> = ux
            .forms
            .iter()
            .map(|form| (form.form_id.as_str(), form))
            .collect();
        let label_overrides = label_overrides(ux);

        facts.declare(ux, &items);
        let context = Context {
            placements: &placements,
            items: &items,
            fields: &fields,
            forms: &forms,
            nullable: nullable_inputs,
            label_overrides,
            composed,
            stack: Vec::new(),
            host: None,
        };
        for (index, page) in ux.pages.iter().enumerate() {
            facts
                .page_pointer
                .insert(page.page_id.clone(), format!("/payload/ux/pages/{index}"));
            if text_is_named(&page.name) {
                facts.page_named.insert(page.page_id.clone());
            }
            facts.walk(
                &context,
                &page.root,
                &format!("/payload/ux/pages/{index}/root"),
                None,
                &page.page_id,
            );
        }
        facts
    }

    /// Declared bindings, placements, forms, navigation items and the app shell (routes and shell items).
    fn declare(&mut self, ux: &WidgetUx, items: &BTreeMap<&str, &norn_ir::WidgetNavigationItem>) {
        for (index, binding) in ux.action_bindings.iter().enumerate() {
            self.action_binding.insert(binding.binding_id.clone());
            self.binding_pointer.insert(
                binding.binding_id.clone(),
                format!("/payload/ux/actionBindings/{index}"),
            );
        }
        for placement in &ux.action_placements {
            self.uses.insert(placement.binding_id.clone());
        }
        for form in &ux.forms {
            self.uses.insert(form.submit_binding_id.clone());
        }
        for (index, item) in ux.navigation_items.iter().enumerate() {
            self.declared_item.insert(item.item_id.clone());
            self.item_pointer.insert(
                item.item_id.clone(),
                format!("/payload/ux/navigationItems/{index}"),
            );
            if never_visible(item.visible_when.as_ref()) {
                self.hidden_item.insert(item.item_id.clone());
            }
            if let WidgetNavigationDestination::Page { page_id, .. } = &item.destination {
                self.item_page.insert(item.item_id.clone(), page_id.clone());
            }
        }
        if let Some(app) = &ux.app {
            self.app_present = true;
            self.entry.insert(app.entry_page_id.clone());
            self.app_page
                .extend(app.page_refs.iter().map(|page| page.page_id.clone()));
            // A route is an entry the host provides (deep link, record page opened elsewhere).
            self.routed_page
                .extend(app.routes.iter().map(|route| route.page_id.clone()));
            for item_id in &app.navigation_item_ids {
                self.shell_item.insert(item_id.clone());
                // A shell item is a control like any other: it needs a name.
                if let Some(item) = items.get(item_id.as_str()) {
                    let element = shell_key(item_id);
                    let pointer = self
                        .item_pointer
                        .get(item_id)
                        .cloned()
                        .unwrap_or_else(|| "/payload/ux".to_owned());
                    self.interactive.insert(element.clone());
                    self.add_label(&element, &item.label);
                    self.node_pointer.insert(element, pointer);
                }
            }
        }
    }

    fn add_label(&mut self, element: &str, text: &LocaleText) {
        if !text.0.is_empty() {
            self.has_label.insert(element.to_owned());
        }
        for (locale, value) in &text.0 {
            self.label
                .insert((element.to_owned(), locale.clone(), text_is_named(value)));
        }
    }

    fn walk(
        &mut self,
        context: &Context<'_>,
        node: &WidgetNode,
        pointer: &str,
        parent: Option<&str>,
        page: &str,
    ) {
        let id = match &context.host {
            Some(host) => composed_key(host, &node.node_id),
            None => node_key(&node.node_id),
        };
        self.node_pointer.insert(id.clone(), pointer.to_owned());
        self.in_page.insert(id.clone(), page.to_owned());
        if let Some(parent) = parent {
            self.parent.insert(id.clone(), parent.to_owned());
        }
        self.classify(context, node, &id);
        if let WidgetComponent::Composition { widget, page_id } = &node.component {
            self.walk_composed(context, &id, pointer, page, widget, page_id);
        }
        for (index, child) in node.children.iter().enumerate() {
            let child_pointer = if context.host.is_some() {
                pointer.to_owned()
            } else {
                format!("{pointer}/children/{index}")
            };
            self.walk(context, child, &child_pointer, Some(&id), page);
        }
    }

    /// The selected page of a composed baseline is part of the host page: its controls are named
    /// with the host's overrides applied, and its dialogs and keyboard paths are judged in the host.
    fn walk_composed(
        &mut self,
        context: &Context<'_>,
        host_node: &str,
        pointer: &str,
        host_page: &str,
        widget: &WidgetArtifactRef,
        page_id: &str,
    ) {
        let identity = (
            widget.id.clone(),
            widget.version.clone(),
            page_id.to_owned(),
        );
        if context.stack.contains(&identity) {
            return;
        }
        let Some(baseline) = (context.composed)(widget) else {
            return;
        };
        let Some(selected) = baseline.pages.iter().find(|page| page.page_id == page_id) else {
            return;
        };
        let placements: BTreeMap<&str, &norn_ir::WidgetActionPlacement> = baseline
            .action_placements
            .iter()
            .map(|placement| (placement.placement_id.as_str(), placement))
            .collect();
        let items: BTreeMap<&str, &norn_ir::WidgetNavigationItem> = baseline
            .navigation_items
            .iter()
            .map(|item| (item.item_id.as_str(), item))
            .collect();
        let fields: BTreeMap<&str, &norn_ir::WidgetFieldPresentation> = baseline
            .fields
            .iter()
            .map(|field| (field.field_id.as_str(), field))
            .collect();
        let forms: BTreeMap<&str, &norn_ir::WidgetForm> = baseline
            .forms
            .iter()
            .map(|form| (form.form_id.as_str(), form))
            .collect();
        // The baseline's own overrides apply first; the host's overrides replace them.
        let mut overrides = label_overrides(&baseline);
        overrides.extend(context.label_overrides.iter().map(|(k, v)| (*k, *v)));
        let no_nullable: BTreeSet<Pair> = BTreeSet::new();
        let mut stack = context.stack.clone();
        stack.push(identity);
        let nested = Context {
            placements: &placements,
            items: &items,
            fields: &fields,
            forms: &forms,
            nullable: &no_nullable,
            label_overrides: overrides,
            composed: context.composed,
            stack,
            host: Some(host_node.to_owned()),
        };
        if text_is_named(&selected.name) {
            self.named_scope.insert(host_node.to_owned());
        }
        self.walk(&nested, &selected.root, pointer, Some(host_node), host_page);
    }

    fn classify(&mut self, context: &Context<'_>, node: &WidgetNode, id: &str) {
        match &node.component {
            WidgetComponent::Section { section } => {
                let label = context.effective(node, &section.label);
                if matches!(section.kind, WidgetSectionKind::Tab) {
                    self.interactive.insert(id.to_owned());
                    self.add_label(id, label);
                }
                if locale_text_is_named(label) {
                    self.named_container.insert(id.to_owned());
                }
            }
            WidgetComponent::Dialog {
                binding_id,
                dismissal_methods,
                ..
            } => {
                self.dialog.insert(id.to_owned());
                for method in dismissal_methods {
                    self.dismissal
                        .insert((id.to_owned(), dismissal_name(*method).to_owned()));
                }
                if let Some(binding) = binding_id
                    && context.host.is_none()
                {
                    self.uses.insert(binding.clone());
                }
            }
            WidgetComponent::Form { form_id } => {
                if let Some(form) = context.forms.get(form_id.as_str()) {
                    self.interactive.insert(id.to_owned());
                    self.add_label(id, context.effective(node, &form.submit.label));
                }
            }
            WidgetComponent::ChildGrid {
                label, field_ids, ..
            } => self.classify_grid(context, node, id, label, field_ids),
            WidgetComponent::Field { field_id } => self.classify_field(context, node, id, field_id),
            WidgetComponent::Action { placement_id } => {
                if let Some(placement) = context.placements.get(placement_id.as_str()) {
                    self.interactive.insert(id.to_owned());
                    self.action_node.insert(id.to_owned());
                    self.add_label(id, context.effective(node, &placement.label));
                }
            }
            WidgetComponent::Navigation { item_id } => {
                if let Some(item) = context.items.get(item_id.as_str()) {
                    self.interactive.insert(id.to_owned());
                    self.add_label(id, context.effective(node, &item.label));
                    if context.host.is_none() {
                        self.nav_node.insert((id.to_owned(), item_id.clone()));
                    }
                }
            }
            WidgetComponent::List { .. }
            | WidgetComponent::Detail { .. }
            | WidgetComponent::Composition { .. } => {}
        }
    }

    /// A child grid is a labelled region; every row repeats its inputs, so each needs its own name.
    fn classify_grid(
        &mut self,
        context: &Context<'_>,
        node: &WidgetNode,
        id: &str,
        label: &LocaleText,
        field_ids: &[String],
    ) {
        let label = context.effective(node, label);
        self.interactive.insert(id.to_owned());
        self.add_label(id, label);
        if locale_text_is_named(label) {
            self.named_container.insert(id.to_owned());
        }
        for field_id in field_ids {
            if let Some(field) = context.fields.get(field_id.as_str())
                && !matches!(field.control, WidgetFieldControl::Display)
            {
                let element = input_key(id, field_id);
                self.interactive.insert(element.clone());
                // A row input is named by its field's label, or the override a host gives that field.
                let label = context
                    .label_overrides
                    .get(field_id.as_str())
                    .copied()
                    .unwrap_or(&field.label);
                self.add_label(&element, label);
                // It lives inside its grid, on the grid's page.
                self.parent.insert(element.clone(), id.to_owned());
                if let Some(page) = self.in_page.get(id).cloned() {
                    self.in_page.insert(element.clone(), page);
                }
                if let Some(pointer) = self.node_pointer.get(id).cloned() {
                    self.node_pointer.insert(element, pointer);
                }
            }
        }
    }

    /// An editable field is an interactive control; record what a clear path needs to know.
    fn classify_field(
        &mut self,
        context: &Context<'_>,
        node: &WidgetNode,
        id: &str,
        field_id: &str,
    ) {
        let Some(field) = context.fields.get(field_id) else {
            return;
        };
        if matches!(field.control, WidgetFieldControl::Display) {
            return;
        }
        self.interactive.insert(id.to_owned());
        self.add_label(id, context.effective(node, &field.label));
        if let WidgetFieldTarget::FormInput {
            form_id,
            input_name,
            ..
        } = &field.target
            && context
                .nullable
                .contains(&(form_id.clone(), input_name.clone()))
        {
            self.nullable_field.insert(id.to_owned());
        }
        if clear_capable(field.control) {
            self.clear_capable.insert(id.to_owned());
        }
        if field
            .required_when
            .as_ref()
            .is_some_and(|condition| condition.expression.as_str().trim() == "true")
        {
            self.always_required.insert(id.to_owned());
        }
    }
}

/// Every ancestor of every node, nearest first. The parent relation is a forest over unique
/// node ids; the length bound only guards a malformed document from looping.
fn ancestors(parent: &BTreeMap<String, String>) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for node in parent.keys() {
        let mut chain = Vec::new();
        let mut current = node;
        while let Some(next) = parent.get(current) {
            if chain.len() > parent.len() {
                break;
            }
            chain.push(next.clone());
            current = next;
        }
        out.insert(node.clone(), chain);
    }
    out
}

/// Least set containing `seed` and closed under `edge(P, Q)`.
fn reachable(seed: &BTreeSet<String>, edges: &BTreeSet<Pair>) -> BTreeSet<String> {
    let mut reached = seed.clone();
    loop {
        let before = reached.len();
        for (from, to) in edges {
            if reached.contains(from) {
                reached.insert(to.clone());
            }
        }
        if reached.len() == before {
            return reached;
        }
    }
}

fn pointer_of(map: &BTreeMap<String, String>, id: &str) -> String {
    map.get(id)
        .cloned()
        .unwrap_or_else(|| "/payload/ux".to_owned())
}

/// Stratum 0: ancestors and reachability. Stratum 1 and 2: rules with negation. Findings last.
fn derive(facts: &Facts) -> Vec<Finding> {
    // rule: ancestor(N, A) :- parent(N, A).  ancestor(N, A) :- parent(N, M), ancestor(M, A).
    let ancestor = ancestors(&facts.parent);
    // rule: control_edge(P, Q) :- nav_node(N, I), !hidden_item(I), in_page(N, P), item_page(I, Q).
    let mut edges = BTreeSet::new();
    for (node, item) in &facts.nav_node {
        if facts.hidden_item.contains(item) {
            continue;
        }
        if let (Some(page), Some(destination)) =
            (facts.in_page.get(node), facts.item_page.get(item))
        {
            edges.insert((page.clone(), destination.clone()));
        }
    }
    // rule: seed(P) :- entry(P).  seed(P) :- routed_page(P).
    // rule: seed(Q) :- shell_item(I), !hidden_item(I), item_page(I, Q).   (the shell is on every page)
    let mut seed = BTreeSet::new();
    if facts.app_present {
        seed.extend(facts.entry.iter().cloned());
        seed.extend(facts.routed_page.iter().cloned());
        for item in &facts.shell_item {
            if !facts.hidden_item.contains(item)
                && let Some(destination) = facts.item_page.get(item)
            {
                seed.insert(destination.clone());
            }
        }
    }
    // rule: reach(P) :- seed(P).  reach(Q) :- reach(P), control_edge(P, Q).
    let reach = reachable(&seed, &edges);

    let mut findings = Vec::new();
    unnamed_controls(facts, &mut findings);
    dialog_rules(facts, &ancestor, &mut findings);
    transition_rules(facts, &reach, &mut findings);
    nullable_rules(facts, &mut findings);
    keyboard_rules(facts, &ancestor, &reach, &mut findings);
    findings
}

// rule: bad_label(E) :- label(E, _, false).
// rule: named(E) :- has_label(E), !bad_label(E).
// rule: unnamed(E) :- interactive(E), !named(E).
fn unnamed_controls(facts: &Facts, findings: &mut Vec<Finding>) {
    let bad: BTreeSet<&String> = facts
        .label
        .iter()
        .filter(|(_, _, named)| !named)
        .map(|(element, _, _)| element)
        .collect();
    for element in &facts.interactive {
        if !facts.has_label.contains(element) || bad.contains(element) {
            findings.push(Finding {
                code: UNNAMED_CONTROL,
                pointer: pointer_of(&facts.node_pointer, element),
                message: format!(
                    "interactive control {:?} needs an accessible name: every localized label must contain a letter or digit",
                    show(element),
                ),
            });
        }
    }
}

// rule: has_dismissal(D) :- dismissal(D, _).
// rule: keyboard_close(D) :- dismissal(D, "visible_close").
// rule: keyboard_close(D) :- dismissal(D, "escape").
// rule: no_keyboard_close(D) :- dialog(D), has_dismissal(D), !keyboard_close(D).
// rule: has_invoker(D) :- ancestor(A, D), action_node(A).
// rule: restore_ok(D) :- has_invoker(D).
// rule: restore_ok(D) :- ancestor(D, C), named_container(C).
// rule: restore_ok(D) :- in_page(D, P), page_named(P).
// rule: restore_ok(D) :- ancestor(D, C), named_scope(C).
// rule: no_restore(D) :- dialog(D), !restore_ok(D).
// rule: too_deep(D) :- dialog(D), count { A : ancestor(D, A), dialog(A) } >= 3.
fn dialog_rules(
    facts: &Facts,
    ancestor: &BTreeMap<String, Vec<String>>,
    findings: &mut Vec<Finding>,
) {
    // has_invoker(D): every action node marks the dialogs above it.
    let mut has_invoker: BTreeSet<&String> = BTreeSet::new();
    for action in &facts.action_node {
        for outer in ancestor.get(action).into_iter().flatten() {
            if facts.dialog.contains(outer) {
                has_invoker.insert(outer);
            }
        }
    }
    let mut has_dismissal: BTreeSet<&String> = BTreeSet::new();
    let mut keyboard_close: BTreeSet<&String> = BTreeSet::new();
    for (node, method) in &facts.dismissal {
        has_dismissal.insert(node);
        if method == "visible_close" || method == "escape" {
            keyboard_close.insert(node);
        }
    }
    for dialog in &facts.dialog {
        let pointer = pointer_of(&facts.node_pointer, dialog);
        if has_dismissal.contains(dialog) && !keyboard_close.contains(dialog) {
            findings.push(Finding {
                code: DIALOG_NO_KEYBOARD_CLOSE,
                pointer: pointer.clone(),
                message: format!(
                    "dialog {:?} can only be dismissed by pointer; declare visible_close or escape",
                    show(dialog),
                ),
            });
        }
        let above: &[String] = match ancestor.get(dialog) {
            Some(chain) => chain.as_slice(),
            None => &[],
        };
        let named_container = above
            .iter()
            .any(|container| facts.named_container.contains(container));
        let named_page = facts
            .in_page
            .get(dialog)
            .is_some_and(|page| facts.page_named.contains(page))
            || above.iter().any(|outer| facts.named_scope.contains(outer));
        if !(has_invoker.contains(dialog) || named_container || named_page) {
            findings.push(Finding {
                code: DIALOG_NO_RESTORE_TARGET,
                pointer: pointer.clone(),
                message: format!(
                    "dialog {:?} has no focus restore target in the document: it needs an action control inside it or a named enclosing region or page",
                    show(dialog),
                ),
            });
        }
        let dialog_ancestors = above
            .iter()
            .filter(|outer| facts.dialog.contains(*outer))
            .count();
        if dialog_ancestors >= VERIFIED_MODAL_DEPTH {
            findings.push(Finding {
                code: DIALOG_DEPTH,
                pointer,
                message: format!(
                    "dialog {:?} nests deeper than the {VERIFIED_MODAL_DEPTH} stacked dialogs the renderer statechart models",
                    show(dialog),
                ),
            });
        }
    }
}

// rule: no_clear(F) :- nullable_field(F), interactive(F), !clear_capable(F).
// rule: no_clear(F) :- nullable_field(F), always_required(F).
fn nullable_rules(facts: &Facts, findings: &mut Vec<Finding>) {
    for field in &facts.nullable_field {
        let cannot_hold_null = !facts.clear_capable.contains(field);
        let forced = facts.always_required.contains(field);
        if cannot_hold_null || forced {
            let why = if forced {
                "its requiredWhen is always true, so a cleared value could never be submitted"
            } else {
                "its control has no clear affordance"
            };
            findings.push(Finding {
                code: NULLABLE_WITHOUT_CLEAR,
                pointer: pointer_of(&facts.node_pointer, field),
                message: format!(
                    "field {:?} is bound to a nullable input but cannot be cleared to null: {why}",
                    show(field),
                ),
            });
        }
    }
}

// rule: unreachable_page(P) :- app_page(P), !reach(P).
// rule: rendered_item(I) :- nav_node(_, I), !hidden_item(I).
// rule: rendered_item(I) :- shell_item(I), !hidden_item(I).
// rule: missing_control(I) :- declared_item(I), !rendered_item(I).
// rule: used_binding(B) :- uses(B).   dead_binding(B) :- action_binding(B), !used_binding(B).
// rule: illegal_control(N) :- nav_node(N, I), item_page(I, Q), app_present, !app_page(Q).
// rule: illegal_control(I) :- shell_item(I), item_page(I, Q), !app_page(Q).
fn transition_rules(facts: &Facts, reach: &BTreeSet<String>, findings: &mut Vec<Finding>) {
    if facts.app_present {
        for page in &facts.app_page {
            if !reach.contains(page) {
                findings.push(Finding {
                    code: UNREACHABLE_PAGE,
                    pointer: pointer_of(&facts.page_pointer, page),
                    message: format!(
                        "page {page:?} has no control or route that reaches it from the app entry page"
                    ),
                });
            }
        }
    }
    let rendered: BTreeSet<&String> = facts
        .nav_node
        .iter()
        .map(|(_, item)| item)
        .chain(facts.shell_item.iter())
        .filter(|item| !facts.hidden_item.contains(*item))
        .collect();
    for item in &facts.declared_item {
        if !rendered.contains(item) {
            findings.push(Finding {
                code: CONTROL_MISSING,
                pointer: pointer_of(&facts.item_pointer, item),
                message: format!(
                    "navigation item {item:?} is declared but no visible control renders it"
                ),
            });
        }
    }
    for binding in &facts.action_binding {
        if !facts.uses.contains(binding) {
            findings.push(Finding {
                code: CONTROL_MISSING,
                pointer: pointer_of(&facts.binding_pointer, binding),
                message: format!(
                    "action binding {binding:?} is declared but no placement, form or dialog uses it"
                ),
            });
        }
    }
    if facts.app_present {
        for (node, item) in &facts.nav_node {
            if offers_page_outside_app(facts, item) {
                findings.push(Finding {
                    code: CONTROL_ILLEGAL,
                    pointer: pointer_of(&facts.node_pointer, node),
                    message: format!(
                        "navigation control {:?} leads to a page the app does not declare",
                        show(node),
                    ),
                });
            }
        }
        for item in &facts.shell_item {
            if offers_page_outside_app(facts, item) {
                findings.push(Finding {
                    code: CONTROL_ILLEGAL,
                    pointer: pointer_of(&facts.item_pointer, item),
                    message: format!(
                        "shell navigation item {item:?} leads to a page the app does not declare"
                    ),
                });
            }
        }
    }
}

fn offers_page_outside_app(facts: &Facts, item: &str) -> bool {
    facts
        .item_page
        .get(item)
        .is_some_and(|page| !facts.app_page.contains(page))
}

// rule: kb_trap(E) :- interactive(E), ancestor(E, D), dialog(D), has_dismissal(D), !keyboard_close(D).
// rule: kb_trap(E) :- interactive(E), in_page(E, P), app_page(P), !reach(P).
fn keyboard_rules(
    facts: &Facts,
    ancestor: &BTreeMap<String, Vec<String>>,
    reach: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    let mut keyboard_close: BTreeSet<&String> = BTreeSet::new();
    let mut dismissable: BTreeSet<&String> = BTreeSet::new();
    for (node, method) in &facts.dismissal {
        dismissable.insert(node);
        if method == "visible_close" || method == "escape" {
            keyboard_close.insert(node);
        }
    }
    for element in &facts.interactive {
        let trapped = ancestor.get(element).into_iter().flatten().any(|outer| {
            facts.dialog.contains(outer)
                && dismissable.contains(outer)
                && !keyboard_close.contains(outer)
        });
        // Only the app's own pages can strand a control; a composed page sits on its host page.
        let stranded = facts
            .in_page
            .get(element)
            .is_some_and(|page| facts.app_page.contains(page) && !reach.contains(page));
        if trapped || stranded {
            let cause = if trapped {
                "sits inside a dialog with no keyboard dismissal"
            } else {
                "is on a page no control or route reaches"
            };
            findings.push(Finding {
                code: KEYBOARD_UNREACHABLE,
                pointer: pointer_of(&facts.node_pointer, element),
                message: format!(
                    "control {:?} is not keyboard reachable: it {cause}",
                    show(element)
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_element_keys_never_collide_with_declared_nodes() {
        // A node literally named like a grid row input, or like a composed child, stays distinct.
        assert_ne!(node_key("g#f"), input_key(&node_key("g"), "f"));
        assert_ne!(node_key("a::b"), composed_key(&node_key("a"), "b"));
        assert_ne!(composed_key("ab", "c"), composed_key("a", "bc"));
        assert_ne!(input_key("ab", "c"), input_key("a", "bc"));
        assert_ne!(shell_key("x"), node_key("x"));
        assert_eq!(show(&node_key("g#f")), "g#f");
        assert_eq!(show(&input_key(&node_key("grid"), "qty")), "grid#qty");
        assert_eq!(
            show(&composed_key(&node_key("host"), "child")),
            "host::child"
        );
    }

    #[test]
    fn a_read_only_display_has_no_clear_path_and_every_input_has_one() {
        assert!(!clear_capable(WidgetFieldControl::Display));
        for control in [
            WidgetFieldControl::Text,
            WidgetFieldControl::TextArea,
            WidgetFieldControl::Number,
            WidgetFieldControl::Date,
            WidgetFieldControl::DateTime,
            WidgetFieldControl::Checkbox,
            WidgetFieldControl::Select,
            WidgetFieldControl::Link,
        ] {
            assert!(clear_capable(control));
        }
    }

    #[test]
    fn a_label_needs_a_letter_or_digit_in_every_locale() {
        assert!(text_is_named("Approve"));
        assert!(text_is_named("1"));
        assert!(!text_is_named("…"));
        let mixed = LocaleText(
            [
                ("en".to_owned(), "Go".to_owned()),
                ("ar".to_owned(), "-".to_owned()),
            ]
            .into_iter()
            .collect(),
        );
        assert!(!locale_text_is_named(&mixed));
    }
}
