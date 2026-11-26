//! Record, selection, draft, and row source availability queries across Widget UX surfaces.

use norn_ir::{
    WidgetActionLocation, WidgetActionPlacement, WidgetComponent, WidgetDataQuery,
    WidgetFieldTarget, WidgetForm, WidgetSelectionMode, WidgetUxActionBinding,
};

use super::super::child_grid::child_grid_component;
use super::super::{ActionSurface, Usage, UxChecker, path_index};

impl UxChecker<'_, '_, '_> {
    pub(in super::super) fn list_rendered_at(&self, binding_id: &str, path: &str) -> bool {
        let pages = self.pages_at(path);
        !pages.is_empty()
            && self.rendered_lists.get(binding_id).is_some_and(|rendered| {
                pages
                    .iter()
                    .all(|page| page.is_some() && rendered.contains(page))
            })
    }

    pub(in super::super) fn selection_available_at(&self, binding_id: &str, path: &str) -> bool {
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

    pub(super) fn record_source_available(&self, binding_id: &str, path: &str) -> bool {
        self.bindings.get(binding_id).is_some_and(|binding| {
            matches!(
                &binding.query,
                WidgetDataQuery::ObjectDetail { .. } | WidgetDataQuery::ObjectRuntimeDetail { .. }
            )
        }) || (self.list_selections.get(binding_id) == Some(&WidgetSelectionMode::Single)
            && self.list_rendered_at(binding_id, path))
    }

    pub(in super::super) fn value_source_available_at(
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

    pub(in super::super) fn record_source_available_for_form(
        &self,
        form: &WidgetForm,
        binding_id: &str,
    ) -> bool {
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

    pub(in super::super) fn draft_source_available_at(&self, form_id: &str, path: &str) -> bool {
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
        let has_uses = placements.clone().next().is_some() || forms.clone().next().is_some();
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

    pub(in super::super) fn record_source_available_at(
        &self,
        binding_id: &str,
        path: &str,
    ) -> bool {
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
}
