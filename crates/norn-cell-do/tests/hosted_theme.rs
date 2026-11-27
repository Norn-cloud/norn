//! Themes as data on a running cell: a `theme` definition goes through the ordinary
//! `:plan`/`:apply` path, is served by `GET /v1/definitions`, and changes that answer, which is
//! what the live feed pushes (a `norn.live.v1` subscription is re-answered after every
//! successful write and sent a snapshot when the answer's digest changes). A refused theme
//! (low contrast) changes nothing, so nothing is pushed; only a definition writer may apply one;
//! and a workspace keeps one theme, replaced by applying under the same id.

use norn_api::cell_definitions::{APPLY_PATH, PLAN_PATH};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::hosted_install::{Cell, json_of, provisioned, user};

const THEME_ID: &str = "workspace.theme";
const VALID_LUMA: &str = include_str!("../../../fixtures/themes/valid-luma.json");
const VALID_POLAR: &str = include_str!("../../../fixtures/themes/valid-polar.json");
const LOW_CONTRAST: &str = include_str!("../../../fixtures/themes/low-contrast.json");
const NAMED_COLOR: &str = include_str!("../../../fixtures/themes/named-color.json");
const INVISIBLE_WARNING: &str = include_str!("../../../fixtures/themes/invisible-warning.json");

fn theme_with_lifecycle(id: &str, version: &str, lifecycle: &str, payload: &Value) -> Value {
    json!({
        "irVersion": "0.1",
        "artifactId": id,
        "kind": "theme",
        "namespace": "workspace",
        "name": "Workspace theme",
        "version": version,
        "lifecycle": lifecycle,
        "scope": { "orgId": "org_a", "environmentId": "prod" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": payload,
    })
}

#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test helper over a known response shape"
)]
fn theme(id: &str, version: &str, payload: &str) -> Value {
    theme_with_lifecycle(
        id,
        version,
        "active",
        &serde_json::from_str::<Value>(payload).unwrap(),
    )
}

/// What a live subscription to the catalog holds: the digest of the answer's status and body,
/// and the active theme it serves (only `lifecycle: "active"` paints; draft, proposed, deprecated,
/// etc. do not).
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test helper over a known response shape"
)]
fn subscribed(cell: &mut Cell) -> (String, Option<Value>) {
    let read = cell.call("GET", "/v1/definitions", Some(&user("member")), "");
    assert_eq!(read.status, 200, "{}", read.body);
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("{}\n{}", read.status, read.body).as_bytes())
    );
    let themes: Vec<Value> = json_of(&read)["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|artifact| {
            artifact["kind"] == "theme" && artifact["lifecycle"].as_str() == Some("active")
        })
        .cloned()
        .collect();
    assert!(
        themes.len() <= 1,
        "one active theme per workspace: {themes:#?}"
    );
    (digest, themes.into_iter().next())
}

fn apply(cell: &mut Cell, role: &str, artifacts: &[Value]) -> norn_cell_do::server::Response {
    let base = cell.version();
    let planned = cell.change(PLAN_PATH, Some(role), artifacts, None);
    if planned.status != 200 {
        return planned;
    }
    cell.change(APPLY_PATH, Some(role), artifacts, Some(base))
}

#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test helper over a known response shape"
)]
fn refusal_codes(response: &norn_cell_do::server::Response) -> Vec<String> {
    let refusal = json_of(response);
    assert_eq!(refusal["code"], "defs.refused", "{refusal}");
    refusal["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_theme_is_applied_served_and_pushed_and_a_refused_one_is_not() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let base = cell.version();
    let (initial, none) = subscribed(&mut cell);
    assert_eq!(none, None, "a new workspace has no theme");

    // A low-contrast theme is refused at plan and at apply, with the contrast code; nothing
    // changes, so the subscription's answer is the same and nothing is pushed.
    let low = [theme(THEME_ID, "0.2.0", LOW_CONTRAST)];
    let planned = cell.change(PLAN_PATH, Some("owner"), &low, None);
    assert_eq!(planned.status, 422, "{}", planned.body);
    assert!(
        refusal_codes(&planned)
            .iter()
            .all(|code| code == "theme.validation.contrast"),
        "{}",
        planned.body
    );
    let applied = cell.change(APPLY_PATH, Some("owner"), &low, Some(base));
    assert_eq!(applied.status, 422, "{}", applied.body);
    assert_eq!(cell.version(), base);
    assert_eq!(subscribed(&mut cell), (initial.clone(), None));

    // Whoever may not apply definitions may not apply a theme: the one opaque denial.
    let luma = [theme(THEME_ID, "0.1.0", VALID_LUMA)];
    for role in ["member", "viewer"] {
        let denied = cell.change(APPLY_PATH, Some(role), &luma, Some(base));
        assert_eq!(denied.status, 403, "{role}: {}", denied.body);
    }
    assert_eq!(cell.version(), base);

    // An admin applies a valid theme: it is installed as the next revision and served, and the
    // catalog answer changes, which is the live snapshot open Studio tabs receive.
    let installed = apply(&mut cell, "admin", &luma);
    assert_eq!(installed.status, 200, "{}", installed.body);
    assert_eq!(json_of(&installed)["outcome"], "installed");
    assert_eq!(cell.version(), base.saturating_add(1));
    let (after_luma, served) = subscribed(&mut cell);
    assert_ne!(after_luma, initial, "the applied theme is pushed");
    let served = served.expect("the theme is served");
    assert_eq!(served["artifactId"], THEME_ID);
    assert_eq!(
        served["payload"],
        serde_json::from_str::<Value>(VALID_LUMA).unwrap()
    );

    // An agent restyles the workspace by applying the same id with a new payload: replaced,
    // served and pushed again, with no reload or redeploy.
    let polar = [theme(THEME_ID, "0.2.0", VALID_POLAR)];
    let replaced = apply(&mut cell, "owner", &polar);
    assert_eq!(replaced.status, 200, "{}", replaced.body);
    let (after_polar, served) = subscribed(&mut cell);
    assert_ne!(after_polar, after_luma);
    assert_eq!(served.unwrap()["payload"]["name"], "Polar Monochrome");

    // A second theme under another id is refused: the workspace's theme is never ambiguous.
    let second = [theme("workspace.theme.second", "0.1.0", VALID_LUMA)];
    let refused = apply(&mut cell, "owner", &second);
    assert_eq!(refused.status, 422, "{}", refused.body);
    assert!(
        refusal_codes(&refused).contains(&"theme.validation.multiple".to_owned()),
        "{}",
        refused.body
    );
    // A refused low-contrast replacement leaves the served theme as it was.
    let refused = apply(&mut cell, "owner", &low);
    assert_eq!(refused.status, 422, "{}", refused.body);
    assert_eq!(subscribed(&mut cell).0, after_polar, "nothing pushed");
}

#[test]
fn hosted_theme_rejects_unknown_fields_named_colors_invisible_warning_and_supports_reset() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let base = cell.version();
    let luma = [theme(THEME_ID, "0.1.0", VALID_LUMA)];
    let installed = apply(&mut cell, "owner", &luma);
    assert_eq!(installed.status, 200, "{}", installed.body);
    let (after_luma, _) = subscribed(&mut cell);

    // Stale expectedVersion is refused with 409 defs.stale (never silently overwrites).
    let stale = cell.change(APPLY_PATH, Some("owner"), &luma, Some(base));
    assert_eq!(stale.status, 409, "{}", stale.body);
    assert_eq!(json_of(&stale)["code"], "defs.stale");

    // Unknown top-level fields on a theme payload are refused fail-closed at both :plan and :apply.
    let before_refusals = cell.version();
    let mut with_unknown: Value = serde_json::from_str(VALID_LUMA).unwrap();
    with_unknown["fontFamily"] = json!("serif");
    let unknown_artifact = [theme_with_lifecycle(
        THEME_ID,
        "0.2.0",
        "active",
        &with_unknown,
    )];
    let planned_unknown = cell.change(PLAN_PATH, Some("owner"), &unknown_artifact, None);
    assert_eq!(planned_unknown.status, 422, "{}", planned_unknown.body);
    assert!(
        refusal_codes(&planned_unknown).contains(&"theme.validation.schema".to_owned()),
        "{}",
        planned_unknown.body
    );
    let applied_unknown = cell.change(
        APPLY_PATH,
        Some("owner"),
        &unknown_artifact,
        Some(before_refusals),
    );
    assert_eq!(applied_unknown.status, 422, "{}", applied_unknown.body);
    assert!(
        refusal_codes(&applied_unknown).contains(&"theme.validation.schema".to_owned()),
        "{}",
        applied_unknown.body
    );
    assert_eq!(cell.version(), before_refusals);

    // Non-canonical named wire colors (e.g. "red") and invisible warning text ("#0000") are
    // refused directly at :apply without advancing the definitions version.
    let named = [theme(THEME_ID, "0.2.0", NAMED_COLOR)];
    let refused_named = cell.change(APPLY_PATH, Some("owner"), &named, Some(before_refusals));
    assert_eq!(refused_named.status, 422, "{}", refused_named.body);
    assert_eq!(
        refusal_codes(&refused_named),
        vec!["theme.validation.invalid_color".to_owned()]
    );
    assert_eq!(cell.version(), before_refusals);
    let invisible = [theme(THEME_ID, "0.2.0", INVISIBLE_WARNING)];
    let refused_warning = cell.change(APPLY_PATH, Some("owner"), &invisible, Some(before_refusals));
    assert_eq!(refused_warning.status, 422, "{}", refused_warning.body);
    assert!(
        refusal_codes(&refused_warning)
            .iter()
            .all(|code| code == "theme.validation.contrast"),
        "{}",
        refused_warning.body
    );
    assert_eq!(cell.version(), before_refusals);

    // Resetting to default deprecates the workspace theme through the exact same :apply path;
    // GET /v1/definitions stops serving an active theme and its digest changes (pushed), and a
    // subsequent theme can be applied cleanly again.
    let reset_envelope = [theme_with_lifecycle(
        THEME_ID,
        "0.2.0",
        "deprecated",
        &json!({ "version": 1, "name": "Default", "baseSkin": "luma" }),
    )];
    let reset = apply(&mut cell, "owner", &reset_envelope);
    assert_eq!(reset.status, 200, "{}", reset.body);
    let (after_reset, cleared) = subscribed(&mut cell);
    assert_ne!(after_reset, after_luma, "reset changes catalog digest");
    assert_eq!(cleared, None, "reset clears the active theme");

    let reapplied = apply(&mut cell, "owner", &[theme(THEME_ID, "0.3.0", VALID_POLAR)]);
    assert_eq!(reapplied.status, 200, "{}", reapplied.body);
    let (after_reapply, served_again) = subscribed(&mut cell);
    assert_ne!(after_reapply, after_reset);
    assert_eq!(served_again.unwrap()["payload"]["name"], "Polar Monochrome");
}

#[test]
fn hosted_draft_and_proposed_themes_do_not_paint_or_block_active_theme() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let luma_payload: Value = serde_json::from_str(VALID_LUMA).unwrap();
    let polar_payload: Value = serde_json::from_str(VALID_POLAR).unwrap();

    let draft = [theme_with_lifecycle(
        "workspace.theme.draft",
        "0.1.0",
        "draft",
        &luma_payload,
    )];
    let applied_draft = apply(&mut cell, "owner", &draft);
    assert_eq!(applied_draft.status, 200, "{}", applied_draft.body);
    assert_eq!(subscribed(&mut cell).1, None, "draft theme must not paint");

    let proposed = [theme_with_lifecycle(
        "workspace.theme.proposed",
        "0.1.0",
        "proposed",
        &polar_payload,
    )];
    let applied_proposed = apply(&mut cell, "owner", &proposed);
    assert_eq!(applied_proposed.status, 200, "{}", applied_proposed.body);
    assert_eq!(
        subscribed(&mut cell).1,
        None,
        "proposed theme must not paint"
    );

    // An active theme under a different id is admitted without conflict and becomes the sole
    // served active theme.
    let active = [theme(THEME_ID, "0.1.0", VALID_LUMA)];
    let installed = apply(&mut cell, "owner", &active);
    assert_eq!(installed.status, 200, "{}", installed.body);
    let served = subscribed(&mut cell).1.expect("active theme is served");
    assert_eq!(served["artifactId"], THEME_ID);
    assert_eq!(served["lifecycle"], "active");

    // A second active theme under another id is still refused.
    let second_active = [theme("workspace.theme.second", "0.1.0", VALID_POLAR)];
    let current = cell.version();
    let refused = cell.change(APPLY_PATH, Some("owner"), &second_active, Some(current));
    assert_eq!(refused.status, 422, "{}", refused.body);
    assert!(
        refusal_codes(&refused).contains(&"theme.validation.multiple".to_owned()),
        "{}",
        refused.body
    );
    assert_eq!(cell.version(), current);
}
