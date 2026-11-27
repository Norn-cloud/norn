//! Theme admission over the golden fixtures shared with Studio (`fixtures/themes/`): each
//! fixture's refusal codes are exactly `expected.json`'s, through every Ring-1 entry point; Ring-0
//! alone fails closed; a workspace holds one theme.

use std::collections::{BTreeMap, BTreeSet};
use std::{fs, path::Path};

use norn_checker::{Diagnostic, EntityArtifactSetContext, codes};
use norn_ir::{ArtifactEnvelope, ArtifactKind};
use norn_ui_checker::{check_artifact, check_artifact_json, check_artifact_set, validate_artifact};
use serde_json::{Value, json};

use crate::TestMust;

fn fixtures() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/themes")
}

fn theme_json(id: &str, payload: &Value) -> Value {
    json!({
        "irVersion": "0.1",
        "artifactId": id,
        "kind": ArtifactKind::Theme,
        "namespace": "workspace",
        "name": "Workspace theme",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": payload,
    })
}

fn theme(id: &str, payload: &Value) -> ArtifactEnvelope {
    serde_json::from_value(theme_json(id, payload)).req("theme envelope")
}

fn error_codes(diagnostics: &[Diagnostic]) -> BTreeSet<String> {
    diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.code.as_str().to_owned())
        .collect()
}

/// Every fixture with its payload and expected codes.
fn golden() -> Vec<(String, Value, BTreeSet<String>)> {
    let dir = fixtures();
    let expected: BTreeMap<String, BTreeSet<String>> =
        serde_json::from_str(&fs::read_to_string(dir.join("expected.json")).req("expected.json"))
            .req("expected.json is a file → codes map");
    let mut files: Vec<String> = fs::read_dir(&dir)
        .req("fixtures/themes")
        .map(|entry| {
            entry
                .req("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| {
            Path::new(name).extension().is_some_and(|ext| ext == "json") && name != "expected.json"
        })
        .collect();
    files.sort();
    assert_eq!(
        files.iter().cloned().collect::<BTreeSet<_>>(),
        expected.keys().cloned().collect::<BTreeSet<_>>(),
        "expected.json names exactly the fixture files"
    );
    files
        .into_iter()
        .map(|name| {
            let payload: Value =
                serde_json::from_str(&fs::read_to_string(dir.join(&name)).req("fixture"))
                    .req("fixture JSON");
            let codes = expected.get(&name).cloned().req("expected codes");
            (name, payload, codes)
        })
        .collect()
}

#[test]
fn every_golden_theme_gets_exactly_its_expected_codes() {
    let golden = golden();
    assert!(golden.len() >= 8, "{} fixtures", golden.len());
    for (name, payload, expected) in golden {
        let artifact = theme("workspace.theme", &payload);
        for (entry, codes) in [
            ("check_artifact", error_codes(&check_artifact(&artifact))),
            (
                "check_artifact_json",
                error_codes(&check_artifact_json(&theme_json(
                    "workspace.theme",
                    &payload,
                ))),
            ),
            (
                "validate_artifact",
                validate_artifact(&artifact)
                    .err()
                    .unwrap_or_default()
                    .iter()
                    .map(|d| d.code.clone())
                    .collect(),
            ),
        ] {
            assert_eq!(codes, expected, "{name} via {entry}");
        }
        for code in &expected {
            assert!(codes::lookup(code).is_some(), "{name}: {code} unregistered");
        }
    }
}

#[test]
fn contrast_is_measured_in_each_scheme_the_theme_applies_to() {
    let read = |name: &str| -> Value {
        serde_json::from_str(&fs::read_to_string(fixtures().join(name)).req("fixture"))
            .req("fixture JSON")
    };
    let contrast = |payload: &Value| -> Vec<String> {
        check_artifact(&theme("workspace.theme", payload))
            .into_iter()
            .filter(|d| d.code.as_str() == "theme.validation.contrast")
            .map(|d| d.message)
            .collect()
    };
    let dark = contrast(&read("dark-only-contrast.json"));
    assert_ne!(dark.len(), 0);
    assert!(dark.iter().all(|m| m.starts_with("dark ")), "{dark:#?}");
    let light = contrast(&read("low-contrast.json"));
    assert_ne!(light.len(), 0);
    assert!(light.iter().all(|m| m.starts_with("light ")), "{light:#?}");
    // Each failing pair is its own finding, not collapsed into one.
    assert!(light.len() > 1, "{light:#?}");

    // Limited to light, a dark-only failure is not measured at all.
    let mut light_only = read("dark-only-contrast.json");
    light_only["appearance"] = json!("light");
    assert_eq!(contrast(&light_only), Vec::<String>::new());
}

#[test]
fn the_server_shape_is_closed() {
    let mut payload: Value = serde_json::from_str(
        &fs::read_to_string(fixtures().join("valid-luma.json")).req("fixture"),
    )
    .req("fixture JSON");
    payload["fontFamily"] = json!("serif");
    payload["variants"]["sepia"] = json!({});
    let found = check_artifact(&theme("workspace.theme", &payload));
    let schema = found
        .iter()
        .filter(|d| d.code.as_str() == "theme.validation.schema")
        .count();
    assert_eq!(schema, 2, "{found:#?}");
    assert_eq!(
        error_codes(&found),
        BTreeSet::from(["theme.validation.schema".to_owned()])
    );
}

#[test]
fn ring_zero_alone_fails_closed_on_a_theme() {
    let valid: Value = serde_json::from_str(
        &fs::read_to_string(fixtures().join("valid-polar.json")).req("fixture"),
    )
    .req("fixture JSON");
    let ring0 = norn_checker::check_artifact(&theme("workspace.theme", &valid));
    assert_eq!(
        error_codes(&ring0),
        BTreeSet::from([codes::THEME_RING1_REQUIRED.to_owned()])
    );
    assert_eq!(
        check_artifact(&theme("workspace.theme", &valid)),
        Vec::new()
    );
}

#[test]
fn a_workspace_holds_one_theme() {
    let valid: Value = serde_json::from_str(
        &fs::read_to_string(fixtures().join("valid-luma.json")).req("fixture"),
    )
    .req("fixture JSON");
    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let one = check_artifact_set(&[theme("workspace.theme", &valid)], &context);
    assert_eq!(error_codes(&one), BTreeSet::new(), "{one:#?}");

    let two = check_artifact_set(
        &[
            theme("workspace.theme", &valid),
            theme("workspace.theme.second", &valid),
        ],
        &context,
    );
    assert_eq!(
        error_codes(&two),
        BTreeSet::from(["theme.validation.multiple".to_owned()]),
        "{two:#?}"
    );

    // A second theme against an installed one is refused the same way.
    let base = [theme("workspace.theme", &valid)];
    let over_base = check_artifact_set(
        &[theme("workspace.theme.second", &valid)],
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
    );
    assert_eq!(
        error_codes(&over_base),
        BTreeSet::from(["theme.validation.multiple".to_owned()]),
        "{over_base:#?}"
    );

    // A deprecated theme no longer occupies the workspace's active slot: another theme may replace it.
    let mut deprecated_json = theme_json("workspace.theme", &valid);
    deprecated_json["lifecycle"] = json!("deprecated");
    let deprecated: ArtifactEnvelope = serde_json::from_value(deprecated_json).req("deprecated");
    let after_reset = check_artifact_set(
        &[theme("workspace.theme.next", &valid)],
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[deprecated],
            tenant: None,
        },
    );
    assert_eq!(error_codes(&after_reset), BTreeSet::new(), "{after_reset:#?}");
}

#[test]
fn invisible_warning_and_named_colors_are_refused() {
    let invisible = check_artifact(&theme(
        "workspace.theme",
        &json!({ "version": 1, "name": "Hidden Warning", "baseSkin": "luma", "colors": { "warning": "#0000" } }),
    ));
    assert_eq!(
        error_codes(&invisible),
        BTreeSet::from(["theme.validation.contrast".to_owned()]),
        "{invisible:#?}"
    );
    assert!(
        invisible
            .iter()
            .any(|d| d.message.contains("light warning (on background)"))
            && invisible
                .iter()
                .any(|d| d.message.contains("dark warning (on card)")),
        "{invisible:#?}"
    );

    let named = check_artifact(&theme(
        "workspace.theme",
        &json!({ "version": 1, "name": "Named Red", "baseSkin": "luma", "colors": { "chart-1": "red" } }),
    ));
    assert_eq!(
        error_codes(&named),
        BTreeSet::from(["theme.validation.invalid_color".to_owned()]),
        "{named:#?}"
    );
}

#[test]
fn studio_palettes_list_one_token_set_for_every_skin_and_scheme() {
    let palettes: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>> =
        serde_json::from_str(
            &fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../apps/studio/src/lib/theme-palettes.json"),
            )
            .req("palettes"),
        )
        .req("palettes JSON");
    let mut sets = palettes
        .values()
        .flat_map(|schemes| schemes.values())
        .map(|tokens| tokens.keys().cloned().collect::<BTreeSet<_>>());
    let first = sets.next().req("a skin");
    assert!(sets.all(|set| set == first));
    assert!(palettes.values().all(|schemes| {
        schemes.keys().cloned().collect::<Vec<_>>() == ["dark".to_owned(), "light".to_owned()]
    }));
}
