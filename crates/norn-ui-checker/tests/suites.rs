//! `norn-ui-checker`'s integration tests, built as one test binary.

#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::panic,
    reason = "integration test helpers construct and index JSON fixtures"
)]
#[path = "ui_interaction.rs"]
mod ui_interaction;

#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::panic,
    reason = "integration test helpers construct and index JSON fixtures"
)]
#[path = "ui_laws.rs"]
mod ui_laws;

#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    reason = "integration test helpers construct and index JSON fixtures"
)]
#[path = "ui_lifecycle.rs"]
mod ui_lifecycle;

#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration test helpers assert diagnostic snapshot parity"
)]
#[path = "widget_snapshots.rs"]
mod widget_snapshots;

#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration test helpers construct and index JSON fixtures"
)]
#[path = "widget_ux.rs"]
mod widget_ux;
