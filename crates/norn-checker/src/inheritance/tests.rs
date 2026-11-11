//! Old algorithm is a test oracle only, never a production fallback.
#![allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "bounded regression fixtures"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;

fn oracle(parents: &[Option<usize>]) -> BTreeSet<usize> {
    let mut cyclic = BTreeSet::new();
    for start in 0..parents.len() {
        let mut seen = BTreeSet::new();
        let mut current = start;
        while let Some(parent) = parents
            .get(current)
            .copied()
            .flatten()
            .filter(|parent| *parent < parents.len())
        {
            if parent == start {
                cyclic.insert(start);
                break;
            }
            if !seen.insert(parent) {
                break;
            }
            current = parent;
        }
    }
    cyclic
}

fn evaluate(
    parents: &[Option<usize>],
    budget: Budget,
) -> Result<(BTreeSet<usize>, Usage), Refusal> {
    let names: Vec<_> = (0..parents.len())
        .map(|index| ObjectTypeName(format!("test.N{index:04}")))
        .collect();
    let unknown = ObjectTypeName("test.Unknown".to_owned());
    let entries = names.iter().enumerate().map(|(index, name)| {
        (
            name,
            parents[index].map(|parent| names.get(parent).unwrap_or(&unknown)),
        )
    });
    let classification = classify(entries, budget)?;
    Ok((
        names
            .iter()
            .enumerate()
            .filter_map(|(index, name)| classification.cyclic.contains(name).then_some(index))
            .collect(),
        classification.usage,
    ))
}

#[test]
fn every_small_single_parent_graph_matches_the_old_oracle() {
    for size in 0_usize..=4 {
        let radix = size + 2; // no parent, each registered node, unknown parent
        for mut encoding in 0..radix.pow(u32::try_from(size).expect("small graph")) {
            let parents: Vec<_> = (0..size)
                .map(|_| {
                    let digit = encoding % radix;
                    encoding /= radix;
                    digit.checked_sub(1)
                })
                .collect();
            assert_eq!(
                evaluate(&parents, Budget::default())
                    .expect("bounded graph")
                    .0,
                oracle(&parents),
                "{parents:?}"
            );
        }
    }
}

#[test]
fn exact_resource_boundaries_are_admitted_and_each_exhaustion_refuses() {
    let parents = [Some(1), Some(0), Some(0), None, Some(9)];
    let (_, usage) = evaluate(&parents, Budget::default()).expect("baseline");
    let exact = Budget {
        types: parents.len(),
        text_bytes: usage.text_bytes,
        steps: usage.steps,
        facts: usage.facts,
        work: usage.work,
    };
    assert_eq!(evaluate(&parents, exact).expect("exact boundary").1, usage);
    for (dimension, budget) in [
        (
            "registered types",
            Budget {
                types: exact.types - 1,
                ..exact
            },
        ),
        (
            "text bytes",
            Budget {
                text_bytes: exact.text_bytes - 1,
                ..exact
            },
        ),
        (
            "frontier steps",
            Budget {
                steps: exact.steps - 1,
                ..exact
            },
        ),
        (
            "materialized facts",
            Budget {
                facts: exact.facts - 1,
                ..exact
            },
        ),
        (
            "work",
            Budget {
                work: exact.work - 1,
                ..exact
            },
        ),
    ] {
        assert_eq!(
            evaluate(&parents, budget)
                .expect_err("must refuse, without partial classification")
                .dimension,
            dimension
        );
    }
}

#[test]
fn generated_program_is_fixed_and_contains_no_clock_or_timeout_tokens() {
    let source = include_str!(concat!(env!("OUT_DIR"), "/inheritance_ascent.rs"));
    for forbidden in ["Instant", "web_time", "run_timeout"] {
        assert!(!source.contains(forbidden));
    }
    // These invocations exercise the actual generated program, not an oracle.
    let mut program = generated::RegisteredInheritanceStep::default();
    program.query_root.push((7, 3));
    program.frontier.push((7, 2));
    program.extends.push((7, 2, 3));
    program.extends.push((8, 2, 2)); // distinct catalog scopes never join
    program.run();
    assert_eq!(program.next, [(7, 3)]);
    assert_eq!(program.inheritance_cycle, [(7, 3)]);
}

// Strictly decode definitions, not arbitrary JSON objects containing a name.
// Shape validity here is not semantic admission of the negative fixtures.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureLayer {
    #[serde(default)]
    object_types: Vec<norn_ir::ObjectTypeDef>,
    #[serde(default)]
    interfaces: Vec<norn_ir::InterfaceDef>,
    #[serde(default)]
    extensions: Vec<norn_ir::ObjectTypeExtension>,
}

impl FixtureLayer {
    fn model(&self) -> crate::EntityModel<'_> {
        crate::EntityModel {
            object_types: &self.object_types,
            interfaces: &self.interfaces,
            extensions: &self.extensions,
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteModelFixture {
    complete_model: FixtureLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Form {
    Object,
    Array,
    Singular,
    Model,
    Interface,
}

fn extract(
    path: &Path,
    value: serde_json::Value,
) -> Result<Option<(Form, FixtureLayer)>, serde_json::Error> {
    let name = path
        .file_name()
        .expect("fixture file name")
        .to_string_lossy();
    let (form, layer) = if value.is_array() || name.ends_with(".object-types.json") {
        (
            Form::Array,
            FixtureLayer {
                object_types: serde_json::from_value(value)?,
                ..FixtureLayer::default()
            },
        )
    } else if value.get("completeModel").is_some() {
        (
            Form::Model,
            serde_json::from_value::<CompleteModelFixture>(value)?.complete_model,
        )
    } else if value.get("objectTypes").is_some() {
        (Form::Model, serde_json::from_value(value)?)
    } else if value.get("objectType").is_some()
        || value.pointer("/payload/objectType").is_some()
        || value.pointer("/payload/interface").is_some()
    {
        let payload = if let Some(payload) = value.get("payload") {
            // An object payload in a relation/other-kind envelope is malformed,
            // not a selected EntityIR catalog. Strict typed decoding below also
            // rejects wrong versions, multiple definition roots and bad fields.
            if value.get("kind").and_then(serde_json::Value::as_str) != Some("entity") {
                return Err(serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "object definition in a non-entity envelope",
                )));
            }
            let _: norn_ir::ArtifactEnvelope = serde_json::from_value(value.clone())?;
            payload.clone()
        } else {
            value
        };
        match serde_json::from_value::<norn_ir::EntityRoot>(payload)? {
            norn_ir::EntityRoot::V2(root) => (
                Form::Singular,
                FixtureLayer {
                    object_types: vec![root.object_type],
                    ..FixtureLayer::default()
                },
            ),
            norn_ir::EntityRoot::V2Interface(root) => (
                Form::Interface,
                FixtureLayer {
                    interfaces: vec![root.interface],
                    ..FixtureLayer::default()
                },
            ),
            norn_ir::EntityRoot::V2Extension(_)
            | norn_ir::EntityRoot::V2PropertyTypes(_)
            | norn_ir::EntityRoot::V2Markings(_)
            | norn_ir::EntityRoot::V1(_) => {
                unreachable!("strict root decoder must preserve the recognized definition form")
            }
        }
    } else if name.ends_with(".object-type.json") {
        (
            Form::Object,
            FixtureLayer {
                object_types: vec![serde_json::from_value(value)?],
                ..FixtureLayer::default()
            },
        )
    } else if name.ends_with(".interface.json") {
        (
            Form::Interface,
            FixtureLayer {
                interfaces: vec![serde_json::from_value(value)?],
                ..FixtureLayer::default()
            },
        )
    } else {
        // Property types, links, extensions and manifests are not object catalogs.
        return Ok(None);
    };
    Ok(Some((form, layer)))
}

fn fixture_graphs(directory: &Path, layers: &mut BTreeMap<PathBuf, (Form, FixtureLayer)>) {
    for entry in std::fs::read_dir(directory).expect("fixture directory") {
        let path = entry.expect("fixture entry").path();
        if path.is_dir() {
            fixture_graphs(&path, layers);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let value = serde_json::from_slice(&std::fs::read(&path).expect("fixture bytes"))
                .expect("JSON fixture");
            if let Some(layer) = extract(&path, value).unwrap_or_else(|error| {
                panic!(
                    "recognized fixture {} does not decode: {error}",
                    path.display()
                )
            }) {
                layers.insert(path, layer);
            }
        }
    }
}

fn registered<'a>(
    base: &'a FixtureLayer,
    local: &'a FixtureLayer,
) -> BTreeMap<&'a ObjectTypeName, Option<&'a ObjectTypeName>> {
    let mut registered = BTreeMap::new();
    // Match Checker::register_base/register_object_types: base wins, the first
    // object name in each layer wins, and base interface names block local types.
    let base_names: BTreeSet<_> = base
        .object_types
        .iter()
        .map(|def| def.name.as_str())
        .chain(base.interfaces.iter().map(|def| def.name.as_str()))
        .collect();
    for def in &base.object_types {
        registered.entry(&def.name).or_insert(def.extends.as_ref());
    }
    for def in &local.object_types {
        if !base_names.contains(def.name.as_str()) {
            registered.entry(&def.name).or_insert(def.extends.as_ref());
        }
    }
    registered
}

fn compare_snapshot(
    label: &str,
    base: &FixtureLayer,
    local: &FixtureLayer,
) -> BTreeSet<ObjectTypeName> {
    let registered = registered(base, local);
    let names: Vec<_> = registered.keys().copied().collect();
    let parents: Vec<_> = registered
        .values()
        .map(|parent| parent.and_then(|parent| names.binary_search(&parent).ok()))
        .collect();
    let actual = classify(
        registered.iter().map(|(&name, &parent)| (name, parent)),
        Budget::default(),
    )
    .unwrap_or_else(|error| panic!("snapshot {label}: {error:?}"));
    let expected: BTreeSet<_> = oracle(&parents)
        .into_iter()
        .map(|index| names[index])
        .collect();
    assert_eq!(actual.cyclic, expected, "snapshot {label}");
    actual.cyclic.into_iter().cloned().collect()
}

fn join_layers(layers: impl IntoIterator<Item = FixtureLayer>) -> FixtureLayer {
    let mut combined = FixtureLayer::default();
    for layer in layers {
        combined.object_types.extend(layer.object_types);
        combined.interfaces.extend(layer.interfaces);
        combined.extensions.extend(layer.extensions);
    }
    combined
}

fn compare_fixture_files(root: &Path, layers: &BTreeMap<PathBuf, (Form, FixtureLayer)>) -> usize {
    let mut coverage = BTreeMap::<Form, usize>::new();
    for (form, _) in layers.values() {
        *coverage.entry(*form).or_default() += 1;
    }
    assert_eq!(
        coverage,
        BTreeMap::from([
            (Form::Object, 13),
            (Form::Array, 2),
            (Form::Singular, 9),
            (Form::Model, 27),
            (Form::Interface, 3),
        ]),
        "every current recognized fixture form must remain covered"
    );
    let layer = |file: &str| {
        layers
            .get(&root.join(file))
            .unwrap_or_else(|| panic!("missing recognized fixture {file}"))
            .1
            .clone()
    };
    // Exactly the declared lifecycle base, not a flattened union of all files.
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("lifecycles/invalid/manifest.json")).expect("lifecycle manifest"),
    )
    .expect("manifest JSON");
    let base = join_layers(
        manifest["base"]
            .as_array()
            .expect("base paths")
            .iter()
            .map(|file| layer(file.as_str().expect("base path"))),
    );
    assert_eq!(base.object_types.len(), 1);
    let cases: BTreeSet<_> = manifest["fixtures"]
        .as_array()
        .expect("manifest cases")
        .iter()
        .map(|case| {
            root.join("lifecycles/invalid")
                .join(case["file"].as_str().expect("case path"))
        })
        .collect();
    assert_eq!(cases.len(), 27);
    assert!(cases.iter().all(|file| {
        layers
            .get(file)
            .is_some_and(|(form, _)| *form == Form::Model)
    }));
    let empty = FixtureLayer::default();
    let mut snapshots = 0;
    let mut base_snapshots = 0;
    for (path, (_, local)) in layers {
        if local.object_types.is_empty() {
            continue;
        } // interface-only files feed explicit base snapshots below
        let selected_base = if cases.contains(path) {
            base_snapshots += 1;
            &base
        } else {
            &empty
        };
        compare_snapshot(&path.display().to_string(), selected_base, local);
        snapshots += 1;
    }
    assert_eq!((snapshots, base_snapshots), (51, 27));
    snapshots
}

fn compare_catalog_combinations(
    root: &Path,
    layers: &BTreeMap<PathBuf, (Form, FixtureLayer)>,
) -> usize {
    let empty = FixtureLayer::default();
    let layer = |file: &str| {
        layers
            .get(&root.join(file))
            .unwrap_or_else(|| panic!("missing recognized fixture {file}"))
            .1
            .clone()
    };
    // The same catalog combinations as technical_view.rs and entity_artifact_set.rs.
    let technical = join_layers([
        layer("technical-view/technical-view.object-types.json"),
        layer("technical-view/business.object-types.json"),
    ]);
    let registry_base = layer("artifact-set/party.interface.entity.json");
    let registry_local = join_layers([
        layer("artifact-set/purchasing-supplier.entity.json"),
        layer("artifact-set/retail-sku.entity.json"),
    ]);
    let registry = join_layers([registry_base.clone(), registry_local.clone()]);
    // The fixture base and tenant subtype used in entity_model.rs.
    let model_base = join_layers(
        [
            "interfaces/party.interface.json",
            "interfaces/order.interface.json",
            "object-types/customer.object-type.json",
            "object-types/employee.object-type.json",
            "object-types/key-account.object-type.json",
            "object-types/sales-order.object-type.json",
            "object-types/billing-profile.object-type.json",
            "object-types/project.object-type.json",
            "object-types/meta-table.object-type.json",
        ]
        .into_iter()
        .map(layer),
    );
    let snapshots = [
        (
            "technical and business types, one local catalog",
            empty.clone(),
            technical,
        ),
        (
            "artifact registry, one local catalog",
            empty.clone(),
            registry.clone(),
        ),
        (
            "artifact registry, interface base and object candidates",
            registry_base,
            registry_local,
        ),
        (
            "artifact registry installed below tenant extension",
            registry,
            empty,
        ),
        (
            "entity model base and tenant subtype",
            model_base,
            layer("object-types/acme-wholesale-customer.object-type.json"),
        ),
    ];
    for (label, base, local) in &snapshots {
        assert!(
            !registered(base, local).is_empty(),
            "meaningful snapshot {label}"
        );
        compare_snapshot(label, base, local);
    }
    snapshots.len()
}

fn assert_extractor_contract() {
    let empty = FixtureLayer::default();
    // Keep the extractor honest about both the wrapper form and refusals.
    let probe = Path::new("probe.json");
    let complete = extract(
        probe,
        serde_json::json!({"completeModel": {"objectTypes": [{"name": "test.A"}]}}),
    )
    .expect("completeModel shape")
    .expect("recognized completeModel");
    assert_eq!(complete.0, Form::Model);
    compare_snapshot("explicit completeModel", &empty, &complete.1);
    for malformed in [
        serde_json::json!([{"name": 7}]),
        serde_json::json!({"objectTypes": [{"name": "test.A", "unexpected": true}]}),
        serde_json::json!({"completeModel": {"objectTypes": "not an array"}}),
        serde_json::json!({"completeModel": {"objectTypes": []}, "base": "must not be silently discarded"}),
        serde_json::json!({"kind": "entity", "payload": {"entityVersion": "bad", "objectType": {"name": "test.A"}}}),
        serde_json::json!({"kind": "relation", "payload": {"entityVersion": "2.0", "objectType": {"name": "test.A"}}}),
    ] {
        assert!(extract(probe, malformed).is_err());
    }
    assert!(
        extract(
            Path::new("bad.object-types.json"),
            serde_json::json!({"name": "test.FalseArray"})
        )
        .is_err()
    );
    assert!(
        extract(
            probe,
            serde_json::json!({"name": "unrelated", "type": "string"})
        )
        .expect("irrelevant shape")
        .is_none()
    );
}

fn assert_registration_precedence() {
    let decode = |value| serde_json::from_value::<FixtureLayer>(value).expect("precedence fixture");
    let precedence_base = decode(serde_json::json!({"objectTypes": [
        {"name": "test.A", "extends": "test.B"}, {"name": "test.A", "extends": "test.A"}, {"name": "test.B"}
    ], "interfaces": [{"name": "test.Blocked"}]}));
    let precedence_local = decode(serde_json::json!({"objectTypes": [
        {"name": "test.A", "extends": "test.A"}, {"name": "test.X", "extends": "test.Y"},
        {"name": "test.X", "extends": "test.X"}, {"name": "test.Y", "extends": "test.X"},
        {"name": "test.Blocked", "extends": "test.Blocked"}, {"name": "test.Descendant", "extends": "test.X"}
    ]}));
    let members = compare_snapshot(
        "base wins and local duplicates keep first registration",
        &precedence_base,
        &precedence_local,
    );
    assert_eq!(
        members,
        BTreeSet::from([
            ObjectTypeName("test.X".to_owned()),
            ObjectTypeName("test.Y".to_owned())
        ])
    );
    let registry = BTreeSet::new();
    let diagnostics = crate::check_entity_model(
        &precedence_local.model(),
        &crate::EntityContext {
            catalog: &[],
            registry: &registry,
            base: precedence_base.model(),
            tenant: None,
        },
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code.as_str()
                == crate::object_types::codes::INHERITANCE_CYCLE)
            .map(|diagnostic| diagnostic
                .span
                .as_ref()
                .expect("cycle span")
                .pointer
                .as_str())
            .collect::<Vec<_>>(),
        ["/objectTypes/1/extends", "/objectTypes/3/extends"]
    );
}

#[test]
fn all_entity_fixture_catalog_snapshots_match_the_oracle() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ir/entity-v2");
    let mut layers = BTreeMap::new();
    fixture_graphs(&root, &mut layers);
    let snapshots =
        compare_fixture_files(&root, &layers) + compare_catalog_combinations(&root, &layers);
    assert_eq!(
        snapshots, 56,
        "meaningful registered fixture catalog snapshots"
    );
    assert_extractor_contract();
    assert_registration_precedence();
}
