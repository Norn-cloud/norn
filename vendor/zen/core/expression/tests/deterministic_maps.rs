//! Norn `deterministic-maps`: object iteration order never depends on a hash seed.
//!
//! Upstream spills `VariableMap` into an ahash `HashMap` above 32 keys, so `keys` and
//! `values` on equivalent inputs return different orders. With the feature, order is
//! insertion order at every size.

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use zen_expression::variable::Variable;
use zen_expression::Isolate;
use zen_types::symbol::Symbol;
use zen_types::variable::VariableMap;

const KEYS: usize = 40;

fn wide_object() -> Value {
    let mut fields = Map::new();
    for i in 0..KEYS {
        fields.insert(format!("k{i:02}"), json!(i));
    }
    Value::Object(fields)
}

fn run(expression: &str, input: Value) -> Value {
    let mut isolate = Isolate::with_environment(Variable::from(json!({ "o": input })));
    isolate.run_standard(expression).unwrap().into()
}

#[test]
fn eight_equivalent_40_key_inputs_yield_one_keys_order() {
    let mut orders = BTreeSet::new();
    for _ in 0..8 {
        orders.insert(run("keys(o)", wide_object()).to_string());
    }
    assert_eq!(orders.len(), 1, "{orders:?}");

    let expected: Vec<String> = (0..KEYS).map(|i| format!("k{i:02}")).collect();
    assert_eq!(run("keys(o)", wide_object()), json!(expected));
}

#[test]
fn values_follow_keys() {
    let mut orders = BTreeSet::new();
    for _ in 0..8 {
        orders.insert(run("values(o)", wide_object()).to_string());
    }
    assert_eq!(orders.len(), 1, "{orders:?}");
    assert_eq!(
        run("values(o)", wide_object()),
        json!((0..KEYS).collect::<Vec<_>>())
    );
}

#[test]
fn order_is_insertion_order_across_the_spill_threshold() {
    // Reverse insertion, so the expected order is neither sorted nor accidental.
    let mut map = VariableMap::new();
    for i in (0..KEYS).rev() {
        map.insert(
            Symbol::from(format!("k{i:02}").as_str()),
            Variable::from(json!(i)),
        );
    }
    let keys: Vec<String> = map.keys().map(|k| k.as_str().to_string()).collect();
    let expected: Vec<String> = (0..KEYS).rev().map(|i| format!("k{i:02}")).collect();
    assert_eq!(keys, expected);

    // Removal keeps the relative order of the remaining keys; re-insertion appends.
    map.remove(&Symbol::from("k20"));
    map.insert(Symbol::from("k20"), Variable::from(json!(20)));
    let keys: Vec<String> = map.keys().map(|k| k.as_str().to_string()).collect();
    let mut expected: Vec<String> = expected.into_iter().filter(|k| k != "k20").collect();
    expected.push("k20".to_string());
    assert_eq!(keys, expected);
}

#[test]
fn keys_of_a_map_built_in_the_vm_are_stable() {
    // A 40-key object literal built by the expression VM, and a merge of two halves.
    let fields: Vec<String> = (0..KEYS).map(|i| format!("k{i:02}: {i}")).collect();
    let literal = format!("keys({{{}}})", fields.join(", "));
    let merged = format!(
        "keys(merge([{{{}}}, {{{}}}]))",
        fields[..20].join(", "),
        fields[20..].join(", ")
    );
    // Only stability is asserted: the VM's literal construction order is upstream's.
    for expression in [literal, merged] {
        let mut orders = BTreeSet::new();
        for _ in 0..8 {
            orders.insert(run(&expression, json!({})).to_string());
        }
        assert_eq!(orders.len(), 1, "{orders:?}");
    }
}
