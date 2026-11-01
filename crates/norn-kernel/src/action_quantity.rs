//! Lower a trusted, closed `ActionIR` quantity effect to the existing v2 contract.
use crate::{ActionCall, ActionRefusal};
use norn_ir::quantity_ledger::{
    LedgerParameterValue, QUANTITY_CELL_VERSION, QuantityOperation, QuantityRequest,
    QuantityResolution,
};
use norn_ir::{ActionQuantityOperation, RuntimeObject, RuntimeValue};

pub(crate) fn prepare_quantity_action<X>(
    call: &ActionCall<'_>,
    input: &RuntimeObject,
) -> Result<Option<QuantityRequest>, ActionRefusal<X>> {
    let Some(effect) = &call.action.quantity_effect else {
        return Ok(None);
    };
    let invalid = |field: &str| ActionRefusal::InvalidInput {
        field: field.to_owned(),
    };
    let text = |field: &str| -> Result<String, ActionRefusal<X>> {
        input
            .get(field)
            .and_then(RuntimeValue::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| invalid(field))
    };
    let versions = input
        .get(&effect.balance_versions_field)
        .ok_or_else(|| invalid(&effect.balance_versions_field))?;
    let balance_versions = norn_ir::runtime::decode(versions)
        .map_err(|_decoding_error| invalid(&effect.balance_versions_field))?;
    let operation = match &effect.operation {
        ActionQuantityOperation::Reserve {
            reservation_id_field,
            template_id,
            parameters,
            ttl_ms,
        } => QuantityOperation::Reserve {
            reservation_id: text(reservation_id_field)?,
            template_id: template_id.clone(),
            parameters: parameters
                .iter()
                .map(|(id, binding)| {
                    Ok(LedgerParameterValue {
                        parameter_id: id.clone(),
                        scale: u8::try_from(binding.scale.0)
                            .map_err(|_range_error| invalid(&binding.field))?,
                        unscaled: text(&binding.field)?,
                    })
                })
                .collect::<Result<_, ActionRefusal<X>>>()?,
            expires_at: call
                .turn
                .now
                .as_millis()
                .checked_add(ttl_ms.0)
                .filter(|_| ttl_ms.0 > 0)
                .ok_or_else(|| invalid("quantityEffect.operation.ttlMs"))?,
        },
        ActionQuantityOperation::PostReservation {
            reservation_id_field,
            posting_id_field,
        } => QuantityOperation::PostReservation {
            reservation_id: text(reservation_id_field)?,
            posting_id: text(posting_id_field)?,
            resolution: QuantityResolution::All,
        },
    };
    Ok(Some(QuantityRequest {
        version: QUANTITY_CELL_VERSION.into(),
        ledger_id: effect.ledger_id.clone(),
        balance_versions,
        operation,
    }))
}
