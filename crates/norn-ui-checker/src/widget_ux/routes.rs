//! Application shell routes, navigation destinations, and page context parameters.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactKind, WidgetDataQuery, WidgetFilterValue, WidgetNavigationDestination, WidgetPage,
    WidgetSelectionMode,
};

use super::UxChecker;
use super::types::{types_compatible, value_type};

impl UxChecker<'_, '_, '_> {
    pub(super) fn check_navigation_and_app(&mut self) {
        self.check_navigation_items();
        self.check_app_shell();
    }

    fn check_navigation_items(&mut self) {
        let navigation_items = self.ux.navigation_items.clone();
        for (index, item) in navigation_items.iter().enumerate() {
            let path = format!("/payload/ux/navigationItems/{index}");
            if item.required_capability.trim().is_empty() {
                self.error(
                    &format!("{path}/requiredCapability"),
                    "navigation capability requirement must not be blank",
                );
            }
            if item.label.is_blank() {
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
    }

    fn check_app_shell(&mut self) {
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
            if !self
                .ux
                .navigation_items
                .iter()
                .any(|item| item.item_id == *id)
            {
                self.error(
                    &format!("{app_path}/navigationItemIds/{index}"),
                    "app references an unknown navigation item",
                );
            }
        }
        for (index, route) in app.routes.iter().enumerate() {
            self.check_app_route(route, &app_pages, &format!("{app_path}/routes/{index}"));
        }
    }

    fn check_app_route(
        &self,
        route: &norn_ir::WidgetRoute,
        app_pages: &BTreeSet<&str>,
        route_path: &str,
    ) {
        let Some(page) = self
            .pages
            .get(route.page_id.as_str())
            .map(|page| (*page).clone())
        else {
            self.error(
                &format!("{route_path}/pageId"),
                "route destination page does not exist",
            );
            return;
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

    pub(super) fn context_parameter(
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

    pub(super) fn navigation_record_contexts(&self, item_id: &str, path: &str) -> BTreeSet<String> {
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
