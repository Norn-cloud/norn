//! The Norn base schema and loading of the projected schema (T5-AZ-1, #2576).
//!
//! The authorization schema is two Cedar schema fragments composed by
//! `cedar-policy`:
//!
//! - the **base fragment** ([`NORN_BASE_SCHEMA`]), owned here: the structural
//!   `Norn::Scope` record and `Norn::Role`;
//! - the **projected fragment**, emitted by `norn-projections` from
//!   `EntityIR` v2 + `ActionIR` (T1-PROJ-3, #2530): one entity type per object
//!   type and one `Norn::Action` per action.
//!
//! The projection contract this crate relies on, and the validator enforces
//! for every compiled policy:
//!
//! - the principal type `Norn::Actor` is declared by the projection (so it
//!   can carry an optional `represents` identity record and inferred trusted
//!   scalar or set attributes), is a member of `Norn::Role`
//!   (`entity Actor in [Role]`), and carries `scope: Norn::Scope`;
//! - an object type `a.b.C` is the Cedar entity type `a::b::C`, and every
//!   resource type carries `scope: Norn::Scope`;
//! - an action with id `x` is `Norn::Action::"x"`, applies to principal
//!   `Norn::Actor`, and its context carries `scope: Norn::Scope`;
//! - projected ontology link roles are optional `{ type, key }` record
//!   attributes (sets for many links); host `acted_by` retains actor references.
//!   The generic schema-only compiler also supports legacy entity link roles.
//!
//! A projected fragment that breaks the contract is not rejected on load; a
//! policy that relies on the missing piece fails admission instead
//! ([`crate::AuthzCode::PolicyInvalid`], or
//! [`crate::AuthzCode::SchemaScopeType`] when a `scope` is mistyped).

use cedar_policy::{Schema, SchemaFragment};

use crate::diag::{AuthzCode, AuthzError};

/// The Norn-owned base Cedar schema fragment (Cedar schema syntax).
///
/// `Norn::ScopeWitness` exists only so admission can prove, by strict type
/// checking, that every `scope` the scope conjunct compares is a
/// `Norn::Scope` ([`crate::AuthzCode::SchemaScopeType`]); it is never a
/// principal, resource, or stored entity.
///
/// `Norn::Scope` is the kernel's five-dimension scope key: `org` is required,
/// the rest are optional. Records compare by exact equality, so an absent
/// dimension matches only an absent dimension; it is never a wildcard.
pub const NORN_BASE_SCHEMA: &str = "namespace Norn {
  type Scope = {
    org: String,
    tenant?: String,
    workspace?: String,
    environment?: String,
    businessUnit?: String
  };
  entity Role;
  entity ScopeWitness = {
    scope: Scope
  };
}
";

/// A composed, validated authorization schema: the Norn base fragment plus
/// one projected fragment.
#[derive(Debug, Clone)]
pub struct AuthzSchema {
    schema: Schema,
}

impl AuthzSchema {
    /// Load a projected fragment in the Cedar JSON schema format (the
    /// `norn-projections` output form) and compose it with the base fragment.
    ///
    /// # Errors
    ///
    /// [`AuthzCode::SchemaParse`] when the fragment does not parse;
    /// [`AuthzCode::SchemaInvalid`] when the fragments do not compose (an
    /// undeclared or duplicate type or action).
    pub fn from_projected_json(projected: &str) -> Result<Self, AuthzError> {
        let fragment = SchemaFragment::from_json_str(projected).map_err(|error| {
            AuthzError::one(
                AuthzCode::SchemaParse,
                format!("projected schema (JSON): {error}"),
            )
        })?;
        Self::compose(fragment)
    }

    /// Load a projected fragment in the Cedar schema syntax and compose it
    /// with the base fragment. Any schema warning rejects the fragment.
    ///
    /// # Errors
    ///
    /// As [`Self::from_projected_json`]; a schema warning is
    /// [`AuthzCode::SchemaInvalid`].
    pub fn from_projected_cedarschema(projected: &str) -> Result<Self, AuthzError> {
        let fragment = parse_cedarschema("projected schema", projected)?;
        Self::compose(fragment)
    }

    /// Validate projected Cedar schema syntax against the Norn base schema
    /// and serialize the fragment in Cedar's JSON schema format.
    ///
    /// This preserves the same fragment that is composed for policy
    /// admission; callers do not need to implement a second Cedar syntax
    /// translator.
    ///
    /// # Errors
    ///
    /// [`AuthzCode::SchemaParse`] for invalid syntax; [`AuthzCode::SchemaInvalid`]
    /// for warnings, composition failures, or serialization errors.
    pub fn projected_json(projected: &str) -> Result<String, AuthzError> {
        let fragment = parse_cedarschema("projected schema", projected)?;
        Self::compose(fragment.clone())?;
        fragment.to_json_string().map_err(|error| {
            AuthzError::one(
                AuthzCode::SchemaInvalid,
                format!("projected schema JSON serialization: {error}"),
            )
        })
    }

    /// Like [`Self::from_projected_cedarschema`] for several projected
    /// fragments (Cedar schema syntax) composed together, for example the
    /// entity projection and a host's system actions. Fragments may share
    /// namespaces; a type or action declared twice is refused.
    ///
    /// # Errors
    ///
    /// As [`Self::from_projected_cedarschema`].
    pub fn from_projected_cedarschema_fragments(projected: &[&str]) -> Result<Self, AuthzError> {
        let mut fragments = Vec::with_capacity(projected.len());
        for (index, text) in projected.iter().enumerate() {
            fragments.push(parse_cedarschema(
                &format!("projected schema fragment {index}"),
                text,
            )?);
        }
        Self::compose_all(fragments)
    }

    fn compose(projected: SchemaFragment) -> Result<Self, AuthzError> {
        Self::compose_all(vec![projected])
    }

    fn compose_all(projected: Vec<SchemaFragment>) -> Result<Self, AuthzError> {
        let base = parse_cedarschema("Norn base schema", NORN_BASE_SCHEMA)?;
        let schema = Schema::from_schema_fragments(std::iter::once(base).chain(projected))
            .map_err(|error| {
                AuthzError::one(AuthzCode::SchemaInvalid, format!("schema: {error}"))
            })?;
        Ok(Self { schema })
    }

    pub(crate) fn cedar(&self) -> &Schema {
        &self.schema
    }
}

fn parse_cedarschema(what: &str, src: &str) -> Result<SchemaFragment, AuthzError> {
    let (fragment, warnings) = SchemaFragment::from_cedarschema_str(src)
        .map_err(|error| AuthzError::one(AuthzCode::SchemaParse, format!("{what}: {error}")))?;
    let warnings: Vec<String> = warnings.map(|warning| warning.to_string()).collect();
    if warnings.is_empty() {
        Ok(fragment)
    } else {
        Err(AuthzError::one(
            AuthzCode::SchemaInvalid,
            format!("{what}: warnings: {}", warnings.join("; ")),
        ))
    }
}
