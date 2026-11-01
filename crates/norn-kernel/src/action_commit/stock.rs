//! Authoritative stock inputs; verification/posting-v1/owned-finalization.md.
use super::{
    ActionAuthorizer, ActionCall, ActionEffectProgram, ActionRefusal, ActionSliceContext, BTreeMap,
    BTreeSet, CellHost, FactKey, FactSlice, JsonObject, JsonValue, LinkReadExpectation,
    ObjectTypeName, ResourceLimitProfile, SliceMeter, TypeCatalog, action_link_snapshot,
    admit_action_fact_record, authorize_slice_type, authorize_target, check_kind, check_scope,
    host_failure, integer_equals, load_action_fact, owned_manifest_key,
    prepare_action_input_for_call, read_manifest_ids, record_action_link_read,
};
use crate::stock_posting::StockPostLine;

/// One master-selected partition, with lines in owned-manifest order.
#[derive(Debug)]
pub struct StockPartitionInput {
    /// Authoritative company, item and warehouse identities.
    pub masters: [FactKey; 3],
    /// Stock, SRBNB and COGS identities selected by the declared paths.
    pub accounts: [String; 3],
    /// Quantity and acquisition cost read from owned lines, in millionths.
    pub lines: Vec<StockPostLine>,
}

/// Read-only proposal inputs. These values grant no write authority.
#[derive(Debug, Default)]
pub struct StockSnapshot {
    /// Every fact used, including the owned manifest and all masters.
    pub facts: FactSlice,
    /// Complete adjacency sets, including empty sets.
    pub links: Vec<LinkReadExpectation>,
    /// Deterministic company/item/warehouse order.
    pub partitions: Vec<StockPartitionInput>,
}

fn invalid<X>(reason: impl Into<String>) -> ActionRefusal<X> {
    ActionRefusal::SliceUnavailable {
        entry: "$stock".into(),
        reason: reason.into(),
    }
}

struct Reader<'a, 'model, 'call, H: CellHost + ?Sized> {
    context: ActionSliceContext<'a, 'model, 'call, H>,
    snapshot: StockSnapshot,
    meter: SliceMeter,
    observed: u32,
    types: BTreeSet<ObjectTypeName>,
    limits: crate::SliceLimits,
}

impl<H: CellHost + ?Sized> Reader<'_, '_, '_, H> {
    fn fact(&mut self, key: &FactKey) -> Result<JsonObject, ActionRefusal<H::Error>> {
        let c = &self.context;
        let state = load_action_fact(
            c.host,
            c.catalog,
            c.call,
            c.effective_input,
            c.authorizer,
            "$stock",
            &key.object_type,
            key,
            0,
            &mut self.snapshot.facts,
            &mut self.meter,
            &mut self.observed,
            &self.limits,
            &mut self.types,
            false,
        )?;
        state.values.ok_or(ActionRefusal::TargetNotFound)
    }

    fn targets(
        &mut self,
        key: &FactKey,
        link: &str,
    ) -> Result<Vec<FactKey>, ActionRefusal<H::Error>> {
        let c = &self.context;
        let definition = c
            .catalog
            .effective_type(&key.object_type)
            .ok_or(ActionRefusal::TargetNotFound)?;
        let relation = definition
            .links
            .iter()
            .find(|r| r.id.as_str() == link)
            .ok_or(ActionRefusal::TargetNotFound)?;
        authorize_slice_type(c.call, c.effective_input, c.authorizer, &relation.target)?;
        let targets = action_link_snapshot(
            &self.context,
            key,
            link,
            "$stock",
            &self.limits,
            self.observed,
            self.meter.bytes(),
        )?;
        let keys = targets
            .iter()
            .map(|target| target.key.target.clone())
            .collect();
        record_action_link_read(&mut self.snapshot.links, key, link, targets)?;
        Ok(keys)
    }

    fn path(
        &mut self,
        start: &FactKey,
        path: &[String],
    ) -> Result<FactKey, ActionRefusal<H::Error>> {
        let depth = u32::try_from(path.len()).unwrap_or(u32::MAX);
        if depth == 0 || depth > self.limits.max_relation_depth {
            return Err(invalid("invalid master path"));
        }
        let mut key = start.clone();
        for link in path {
            let targets = self.targets(&key, link)?;
            let [target] = targets.as_slice() else {
                return Err(invalid("required master link must have one target"));
            };
            key = target.clone();
            self.fact(&key)?;
        }
        Ok(key)
    }

    fn account(
        &mut self,
        line: &FactKey,
        path: &[String],
    ) -> Result<String, ActionRefusal<H::Error>> {
        let Some((property, links)) = path.split_last() else {
            return Err(invalid("empty account path"));
        };
        let master = self.path(line, links)?;
        text(&self.fact(&master)?, property)
    }
}

fn text<X>(values: &JsonObject, property: &str) -> Result<String, ActionRefusal<X>> {
    values
        .get(property)
        .and_then(JsonValue::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("missing master property {property}")))
}

fn millionths<X>(values: &JsonObject, property: &str) -> Result<u128, ActionRefusal<X>> {
    let Some(JsonValue::Number(value)) = values.get(property) else {
        return Err(invalid("missing stock decimal"));
    };
    let scale = 6_u32
        .checked_sub(value.scale())
        .ok_or_else(|| invalid("stock decimal exceeds scale six"))?;
    value
        .canonical()
        .replace('.', "")
        .parse::<u128>()
        .ok()
        .and_then(|n| n.checked_mul(10_u128.pow(scale)))
        .ok_or_else(|| invalid("stock decimal must be nonnegative scale six"))
}

/// Read and authorize owned lines and declared master/account paths.
///
/// # Errors
/// Invalid, unauthorized or over-bound reads refuse without writes. Retain all fences.
#[allow(
    clippy::too_many_lines,
    reason = "one bounded snapshot preparation pass"
)]
pub fn prepare_stock_snapshot<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
) -> Result<StockSnapshot, ActionRefusal<H::Error>> {
    check_scope(call)?;
    let input = prepare_action_input_for_call::<H::Error>(
        call.action,
        &call.input,
        call.action_artifact_id,
        call.action_version,
    )?
    .effective_input;
    let Some(binding) = &call.action.stock_post else {
        return Err(invalid("missing stock binding"));
    };
    let Some(ActionEffectProgram::FireTransition {
        object_kind,
        object_id_field,
        ..
    }) = &call.action.effect_program
    else {
        return Err(invalid("stock posting requires a transition"));
    };
    let target = FactKey::new(
        call.object_type
            .as_ref()
            .map_or(object_kind.as_str(), ObjectTypeName::as_str),
        text(&input, object_id_field)?,
    );
    check_kind(catalog, &target.object_type, object_kind)?;
    let state = authorize_target(host, call, &input, authorizer, &target)?;
    let limits = ResourceLimitProfile::norn_limits_v1().slice;
    let mut reader = Reader {
        context: ActionSliceContext {
            host,
            catalog,
            call,
            effective_input: &input,
            authorizer,
        },
        snapshot: StockSnapshot {
            facts: FactSlice::from([(target.clone(), state)]),
            ..StockSnapshot::default()
        },
        meter: SliceMeter::new(limits),
        observed: 0,
        types: BTreeSet::new(),
        limits,
    };
    reader.fact(&target)?;
    let definition = catalog
        .effective_type(&target.object_type)
        .ok_or_else(|| invalid("missing voucher type"))?;
    let collection = definition
        .owned_collections
        .iter()
        .find(|c| c.id.as_str() == binding.collection)
        .ok_or_else(|| invalid("missing owned collection"))?;
    authorize_slice_type(call, &input, authorizer, &collection.child_type)?;
    let manifest_key = owned_manifest_key::<H::Error>(&target, &binding.collection)?;
    let manifest = host
        .load_bounded(
            &manifest_key,
            limits
                .max_canonical_bytes
                .saturating_sub(reader.meter.bytes()),
        )
        .map_err(host_failure)?
        .ok_or_else(|| invalid("bounded manifest read unavailable"))?
        .state;
    admit_action_fact_record(
        manifest.values.as_ref(),
        &mut reader.meter,
        &mut reader.observed,
        0,
        &limits,
        "$stock",
    )?;
    let rows = read_manifest_ids(&manifest, &target, &binding.collection, collection)?;
    reader.snapshot.facts.insert(manifest_key, manifest);
    let targets = reader.targets(&target, &binding.collection)?;
    let expected: BTreeSet<_> = rows
        .iter()
        .map(|id| FactKey::new(collection.child_type.as_str(), id.clone()))
        .collect();
    if targets.into_iter().collect::<BTreeSet<_>>() != expected || rows.is_empty() {
        return Err(invalid(
            "owned manifest and parent links must agree and contain lines",
        ));
    }
    let company = reader.path(&target, &["company".into()])?;
    let mut partitions = BTreeMap::<[FactKey; 3], StockPartitionInput>::new();
    for (index, id) in rows.iter().enumerate() {
        let key = FactKey::new(collection.child_type.as_str(), id.clone());
        let values = reader.fact(&key)?;
        if text(&values, collection.row_id_property.as_str())? != *id
            || !integer_equals(
                values.get(collection.ordering_key.as_str()),
                index.saturating_add(1),
            )
        {
            return Err(invalid(
                "owned line identity or order differs from manifest",
            ));
        }
        let item = reader.path(&key, &binding.partition.item)?;
        let warehouse = reader.path(&key, &binding.partition.warehouse)?;
        if item.object_type.as_str() != "stock.Item"
            || warehouse.object_type.as_str() != "stock.Warehouse"
            || reader.path(&warehouse, &["company".into()])? != company
        {
            return Err(invalid("stock partition masters disagree"));
        }
        if text(&reader.fact(&item)?, "valuation_method")? != "fifo" {
            return Err(invalid("CP1 requires FIFO valuation"));
        }
        let accounts = [
            reader.account(&key, &binding.accounts.stock)?,
            reader.account(&key, &binding.accounts.srbnb)?,
            reader.account(&key, &binding.accounts.cogs)?,
        ];
        let masters = [company.clone(), item, warehouse];
        let partition = partitions
            .entry(masters.clone())
            .or_insert(StockPartitionInput {
                masters,
                accounts,
                lines: vec![],
            });
        partition.lines.push(StockPostLine {
            id: id.clone(),
            quantity: millionths(&values, "quantity")?,
            unit_cost: match binding.operation {
                norn_ir::ActionStockOperation::Receipt => Some(millionths(&values, "rate")?),
                norn_ir::ActionStockOperation::Delivery => None,
            },
        });
    }
    reader.snapshot.partitions = partitions.into_values().collect();
    Ok(reader.snapshot)
}

#[cfg(test)]
mod tests {
    use super::{JsonObject, JsonValue, millionths};
    use norn_ir::Decimal;
    #[test]
    fn post_value_exact_millionths() -> Result<(), norn_ir::RuntimeValueError> {
        for (input, expected) in [("1e-6", Some(1)), ("-1", None), ("1e-7", None)] {
            let values =
                JsonObject::from([("quantity".into(), JsonValue::Number(Decimal::parse(input)?))]);
            assert_eq!(
                millionths::<std::convert::Infallible>(&values, "quantity").ok(),
                expected
            );
        }
        Ok(())
    }
}
