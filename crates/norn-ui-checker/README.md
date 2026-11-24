# `norn-ui-checker`

Ring-1 UI and Widget UX checker extracted out of Ring-0 `norn-checker` (`#1613`).

## Responsibility

Validates widget artifact payloads (`envelope`), typed Widget UX contracts (`widget_ux`),
interaction accessibility/reachability rules (`ui_interaction`), page statecharts
(`ui_machine`), semantic views and behaviors (`ui_view`), catalog component contracts
(`ui_catalog`), and page-to-backend lifecycle composition (`ui_lifecycle`).

Delegates semantic graph, expression, lifecycle, and soundness verification to Ring-0
`norn-checker`, `norn-expr`, and `norn-ir`, keeping Ring-0 strictly free of UI/widget
checking modules.

## Boundary

- Ring: `One`
- Allowed `norn-*` dependencies: `norn-checker`, `norn-expr`, `norn-ir`
- Zero host/runtime/IO dependencies; pure deterministic checking (`wasm32`-clean).
