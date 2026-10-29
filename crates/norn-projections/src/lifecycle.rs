//! Declared lifecycle → Mermaid state diagram (ruling 2026-09-25 §2: the
//! declared transition list "is what renders the lifecycle diagram").
//!
//! A pure, one-way text projection of a checked [`Lifecycle`]: an initial
//! pseudo-state edge into each initial state, one edge per `(from, to)` pair
//! labelled with the transition id, and an edge from each terminal state to
//! the final pseudo-state. Guards are not rendered.
//!
//! The business-facing artifact (T3-LIFE-2, #2673) is one Mermaid file per
//! lifecycle, [`lifecycle_diagram`], at
//! `projections/lifecycles/<type>.<property>.mmd`
//! ([`lifecycle_diagram_path`]), titled with the type and property. It is
//! rendered from the same transition list the checker enforces on every
//! decision and action write (`norn_checker::lifecycle_writes`): for a
//! declared lifecycle over the type's own enum,
//! [`declared_lifecycle_diagrams`]; for an inherited or narrowed one, the
//! caller passes the checker's effective lifecycle
//! (`norn_checker::EffectiveObjectType::lifecycles`). The goldens are
//! `fixtures/projections/lifecycles/`. The admin display is T7-ADM-1 (#2621).

use std::fmt::Write as _;

use norn_ir::{Lifecycle, ObjectTypeDef, PropertyType};

/// Where lifecycle diagram artifacts live, relative to a projection root.
pub const LIFECYCLE_DIAGRAM_DIR: &str = "projections/lifecycles";

/// One business-facing lifecycle diagram artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleDiagram {
    /// `projections/lifecycles/<type>.<property>.mmd`.
    pub path: String,
    /// The Mermaid document: a `title` front matter, then the
    /// [`lifecycle_state_diagram`].
    pub mermaid: String,
}

/// The artifact path of the diagram of `object_type`'s lifecycle of
/// `property`. Type names and property ids have closed grammars (dotted
/// names, `[a-z][a-z0-9_]*`), so the path needs no escaping.
#[must_use]
pub fn lifecycle_diagram_path(object_type: &str, property: &str) -> String {
    format!("{LIFECYCLE_DIAGRAM_DIR}/{object_type}.{property}.mmd")
}

/// The diagram artifact of one lifecycle of `object_type`, over `states` (the
/// lifecycle property's effective enum values; see
/// [`lifecycle_state_diagram`]).
#[must_use]
pub fn lifecycle_diagram(
    object_type: &str,
    states: &[&str],
    lifecycle: &Lifecycle,
) -> LifecycleDiagram {
    let property = lifecycle.property.as_str();
    LifecycleDiagram {
        path: lifecycle_diagram_path(object_type, property),
        mermaid: format!(
            "---\ntitle: {object_type} {property} lifecycle\n---\n{}",
            lifecycle_state_diagram(states, lifecycle)
        ),
    }
}

/// The diagram artifacts of the lifecycles `def` declares over its own enum
/// properties, in declaration order. A lifecycle over an inherited property
/// has no local states and is skipped: render it from the checker's
/// effective lifecycle.
#[must_use]
pub fn declared_lifecycle_diagrams(def: &ObjectTypeDef) -> Vec<LifecycleDiagram> {
    def.lifecycles
        .iter()
        .filter_map(|lifecycle| {
            let property = def
                .properties
                .iter()
                .find(|property| property.id == lifecycle.property)?;
            let PropertyType::Enum { values } = &property.property_type else {
                return None;
            };
            let states: Vec<&str> = values.iter().map(|value| value.value.as_str()).collect();
            Some(lifecycle_diagram(def.name.as_str(), &states, lifecycle))
        })
        .collect()
}

/// Render `lifecycle` as a Mermaid `stateDiagram-v2`.
///
/// `states` are the lifecycle property's effective enum values in
/// presentation order (the lifecycle does not restate them). Each state is
/// declared as `state "<value>" as s<index>`, so any enum value renders; a
/// name the lifecycle uses that is not in `states` (an unchecked lifecycle)
/// is appended after them.
#[must_use]
pub fn lifecycle_state_diagram(states: &[&str], lifecycle: &Lifecycle) -> String {
    let mut names: Vec<&str> = states.to_vec();
    let used = lifecycle.initial.iter().chain(&lifecycle.terminal).chain(
        lifecycle.transitions.iter().flat_map(|transition| {
            transition
                .from
                .iter()
                .chain(std::iter::once(&transition.to))
        }),
    );
    for name in used {
        if !names.contains(&name.as_str()) {
            names.push(name);
        }
    }
    let node = |name: &str| {
        names
            .iter()
            .position(|state| *state == name)
            .map_or_else(String::new, |index| format!("s{index}"))
    };

    let mut out = String::from("stateDiagram-v2\n");
    for (index, name) in names.iter().enumerate() {
        // Infallible: writing to a String.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = writeln!(out, "    state \"{}\" as s{index}", escape(name));
    }
    for state in &lifecycle.initial {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = writeln!(out, "    [*] --> {}", node(state));
    }
    for transition in &lifecycle.transitions {
        for from in &transition.from {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "fmt::Write for String is infallible; its trait signature still returns Result"
            )]
            let _ = writeln!(
                out,
                "    {} --> {} : {}",
                node(from),
                node(&transition.to),
                escape(transition.id.as_str())
            );
        }
    }
    for state in &lifecycle.terminal {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = writeln!(out, "    {} --> [*]", node(state));
    }
    out
}

/// Mermaid entity-escape the characters that end a label or a statement.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '#' => out.push_str("#35;"),
            '"' => out.push_str("#quot;"),
            ';' => out.push_str("#59;"),
            ':' => out.push_str("#58;"),
            '\n' | '\r' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}
