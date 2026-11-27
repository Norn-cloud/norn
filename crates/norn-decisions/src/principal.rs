//! PRINCIPAL-INPUT-1 (#1091): the host-resolved `principal` member of a
//! `decide` input.
//!
//! A decide rule declares the principal attributes it reads
//! ([`norn_ir::rule::PrincipalInputDeclaration`]); its `inputSchema` then
//! carries the closed record `principal` with exactly one required column
//! per declaration. The host resolves the values and binds them to the typed
//! event ([`AdmittedDecision::bind_principal`]); an evaluation of a decision
//! that declares principal inputs without bound values produces no effects
//! ([`DecisionError::PrincipalInputUnavailable`]). There is no default.

use std::collections::BTreeSet;
use std::sync::Arc;

use norn_expr::Value;
use norn_ir::RuleRoot;
use norn_ir::rule::{DECIDE_PRINCIPAL_MEMBER, PrincipalInputDeclaration};

use crate::contract::invalid_schema;
use crate::schema::Schema;
use crate::{AdmissionError, AdmittedDecision, DecisionError, TypedEvent};

const PRINCIPAL_PATH: &str = "inputSchema.properties.principal";

/// The declared principal inputs of one admitted decision and the closed
/// record their resolved values are checked against.
#[derive(Clone, Debug)]
pub(crate) struct PrincipalContract {
    schema: Arc<Schema>,
    inputs: Vec<PrincipalInputDeclaration>,
}

impl PrincipalContract {
    pub(crate) const fn schema(&self) -> &Arc<Schema> {
        &self.schema
    }

    /// Pair the rule's declarations with its `principal` input record: both
    /// are present or both absent, and the record has exactly one required
    /// column per declaration.
    pub(crate) fn admit(
        rule: &RuleRoot,
        schema: Option<Schema>,
    ) -> Result<Option<Self>, AdmissionError> {
        let refuse = |reason: &str| invalid_schema(&rule.rule_id, PRINCIPAL_PATH, reason);
        let Some(schema) = schema else {
            return if rule.principal_inputs.is_empty() {
                Ok(None)
            } else {
                Err(refuse(
                    "declared principalInputs require the closed `principal` input record",
                ))
            };
        };
        let properties = schema
            .properties()
            .ok_or_else(|| refuse("must be a closed record"))?;
        let declared: BTreeSet<&str> = rule
            .principal_inputs
            .iter()
            .map(|input| input.name.as_str())
            .collect();
        let columns: BTreeSet<&str> = properties.keys().map(String::as_str).collect();
        let required: BTreeSet<&str> = schema
            .required()
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        if declared.is_empty()
            || declared.len() != rule.principal_inputs.len()
            || declared != columns
            || required != columns
        {
            return Err(refuse(
                "must declare exactly one required column per principalInputs entry",
            ));
        }
        Ok(Some(Self {
            schema: Arc::new(schema),
            inputs: rule.principal_inputs.clone(),
        }))
    }
}

impl AdmittedDecision {
    /// The principal inputs this decision declares, in declaration order;
    /// empty when it reads nothing about the principal.
    #[must_use]
    pub fn principal_inputs(&self) -> &[PrincipalInputDeclaration] {
        self.principal_contract()
            .map_or_default(|contract| contract.inputs.as_slice())
    }

    /// Binds the host-resolved principal inputs to `event`. `principal` is
    /// the record of every declared input by name.
    ///
    /// # Errors
    ///
    /// [`DecisionError::InputBinding`] when `event` was validated for another
    /// decision or schema; [`DecisionError::PrincipalInputType`] when the
    /// decision declares no principal inputs or a value does not conform to
    /// its declared column. The error names the column and never carries a
    /// value.
    pub fn bind_principal(
        &self,
        mut event: TypedEvent,
        principal: Value,
    ) -> Result<TypedEvent, DecisionError> {
        if !self.accepts_event(&event) {
            return Err(DecisionError::InputBinding {
                decision: self.rule_id().to_owned(),
                validated_for: event.rule_id,
            });
        }
        let Some(contract) = self.principal_contract() else {
            return Err(DecisionError::PrincipalInputType {
                path: DECIDE_PRINCIPAL_MEMBER.to_owned(),
            });
        };
        contract
            .schema
            .check(&principal, DECIDE_PRINCIPAL_MEMBER)
            .map_err(|violation| DecisionError::PrincipalInputType {
                path: violation.path,
            })?;
        event.principal = Some((Arc::clone(&contract.schema), principal));
        Ok(event)
    }
}
