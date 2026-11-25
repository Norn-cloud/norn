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

#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test helper over a known response shape"
)]
fn theme(id: &str, version: &str, payload: &str) -> Value {
    json!({
        "irVersion": "0.1",
        "artifactId": id,
        "kind": "theme",
        "namespace": "workspace",
        "name": "Workspace theme",
        "version": version,
        "lifecycle": "active",
        "scope": { "orgId": "org_a", "environmentId": "prod" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": serde_json::from_str::<Value>(payload).unwrap(),
    })
}

/// What a live subscription to the catalog holds: the digest of the answer's status and body,
/// and the theme it serves.
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
        .filter(|artifact| artifact["kind"] == "theme")
        .cloned()
        .collect();
    assert!(themes.len() <= 1, "one theme per workspace: {themes:#?}");
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
