//! Authoritative stock inputs; verification/posting-v1/owned-finalization.md.
use super::{
    ActionAuthorizer, ActionCall, ActionEffectProgram, ActionRefusal, ActionSliceContext, BTreeMap,
    BTreeSet, CellHost, FactKey, FactSlice, JsonObject, JsonValue, LinkReadExpectation,
    ObjectTypeName, ResourceLimitProfile, SliceMeter, TypeCatalog, action_link_snapshot,
    admit_action_fact_record, authorize_slice_type, authorize_target, check_kind, check_scope,
    host_failure, integer_equals, load_action_fact, owned_manifest_key,
    prepare_action_input_for_call, read_manifest_ids, record_action_link_read,
};
use crate::stock_fifo::{FifoError, FifoLayer, FifoMovement, replay_fifo};
use crate::stock_posting::{
    StockPostBindings, StockPostError, StockPostLine, StockPostOperation, StockPostPlan,
    lower_stock_posting,
};

/// Activation ceiling; complete history always fits beside bounded master/line work.
pub const MAX_STOCK_HISTORY_EVENTS: usize = 256;

fn history_capacity<X>(
    prior: usize,
    added: usize,
    non_event: u32,
    records: u32,
) -> Result<(), ActionRefusal<X>> {
    if prior
        .checked_add(added)
        .is_none_or(|count| count > MAX_STOCK_HISTORY_EVENTS)
        || u64::from(non_event)
            .saturating_add(u64::try_from(MAX_STOCK_HISTORY_EVENTS).unwrap_or(u64::MAX))
            > u64::from(records)
    {
        return Err(invalid(
            "stock history capacity exceeded; posting activation refused",
        ));
    }
    Ok(())
}

/// One master-selected partition, with lines in owned-manifest order.
#[derive(Debug)]
pub struct StockPartitionInput {
    /// Authoritative company, item and warehouse identities.
    pub masters: [FactKey; 3],
    /// Stock, SRBNB and COGS identities selected by the declared paths.
    pub accounts: [String; 3],
    /// Quantity and acquisition cost read from owned lines, in millionths.
    pub lines: Vec<StockPostLine>,
    /// Fenced partition identity, including an absent first-post witness.
    pub witness: FactKey,
    /// Prior movements in dense partition order.
    pub history: Vec<FifoMovement>,
    /// Master-selected closed runtime bindings.
    pub bindings: StockPostBindings,
    /// Authoritative quantity unit.
    pub uom: String,
    /// Immutable posting configuration/UOM pin checked against prior history.
    pub binding_pin: String,
    /// Canonical custody party; absent for company inventory.
    pub owner: Option<FactKey>,
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
    fn state(&mut self, key: &FactKey) -> Result<super::FactState, ActionRefusal<H::Error>> {
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
        Ok(state)
    }

    fn fact(&mut self, key: &FactKey) -> Result<JsonObject, ActionRefusal<H::Error>> {
        self.state(key)?.values.ok_or(ActionRefusal::TargetNotFound)
    }

    fn targets(
        &mut self,
        key: &FactKey,
        link: &str,
    ) -> Result<Vec<FactKey>, ActionRefusal<H::Error>> {
        let c = &self.context;
        let model = crate::CommitModel::new(c.catalog);
        let relation = model
            .link(&key.object_type, link)
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

    fn history(
        &mut self,
        partition: &FactKey,
        binding_pin: &str,
    ) -> Result<Vec<FifoMovement>, ActionRefusal<H::Error>> {
        let witness = self.state(partition)?;
        if let Some(values) = &witness.values
            && text::<H::Error>(values, "binding_pin")? != binding_pin
        {
            return Err(invalid("stock binding changed after first posting"));
        }
        history_capacity(0, 0, self.meter.records(), self.limits.max_records)?;
        let keys = self.targets(partition, "events")?;
        let count = keys.len();
        history_capacity(count, 0, self.meter.records(), self.limits.max_records)?;
        if witness
            .values
            .as_ref()
            .is_some_and(|v| text::<H::Error>(v, "id").ok().as_ref() != Some(&partition.key))
        {
            return Err(invalid("partition witness identity mismatch"));
        }
        if !integer_equals(
            witness.values.as_ref().and_then(|v| v.get("event_count")),
            count,
        ) && (witness.exists() || count != 0)
        {
            return Err(invalid("partition event count mismatch"));
        }
        let mut events = BTreeMap::new();
        for key in keys {
            let values = self.fact(&key)?;
            let (ordinal, movement) = history_event(partition, &key, &values)?;
            if events.insert(ordinal, movement).is_some() {
                return Err(invalid("duplicate partition ordinal"));
            }
        }
        if events
            .keys()
            .copied()
            .ne(0..u64::try_from(count).unwrap_or(u64::MAX))
        {
            return Err(invalid("partition event order is not dense"));
        }
        Ok(events.into_values().collect())
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
        let model = crate::CommitModel::new(self.context.catalog);
        let canonical = model.canonical(&key);
        self.snapshot
            .facts
            .keys()
            .find(|candidate| model.canonical(candidate) == canonical)
            .cloned()
            .ok_or_else(|| invalid("missing concrete master snapshot"))
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
    let model = crate::CommitModel::new(catalog);
    let item_type = ObjectTypeName("stock.Item".into());
    let warehouse_type = ObjectTypeName("stock.Warehouse".into());
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
        if !model.is_a(&item.object_type, &item_type)
            || !model.is_a(&warehouse.object_type, &warehouse_type)
            || reader.path(&warehouse, &["company".into()])? != company
        {
            return Err(invalid("stock partition masters disagree"));
        }
        if text(&reader.fact(&item)?, "valuation_method")? != "fifo" {
            return Err(invalid("CP1 requires FIFO valuation"));
        }
        let lot = reader.fact(&item)?;
        let is_metal = model.is_a(
            &item.object_type,
            &ObjectTypeName("jewelry.MetalLot".into()),
        );
        let custody = matches!(
            binding.operation,
            norn_ir::ActionStockOperation::CustodyReceipt
                | norn_ir::ActionStockOperation::CustodyReturn
        );
        let mut owner = None;
        if is_metal {
            let ownership = text::<H::Error>(&lot, "ownership")?;
            if ownership == "custody" && !custody {
                return Err(ActionRefusal::StockPosting(StockPostError::CustodySale));
            }
            if !["custody", "company"].contains(&ownership.as_str()) {
                return Err(invalid("metal ownership must be company or custody"));
            }
            if custody {
                if ownership != "custody" {
                    return Err(invalid("custody posting requires customer-owned metal"));
                }
                let party = reader.path(&item, &["custody_owner".into()])?;
                if model.canonical(&party)
                    != model.canonical(&reader.path(&target, &["owner".into()])?)
                {
                    return Err(invalid("custody voucher owner differs from lot owner"));
                }
                owner = Some(model.canonical(&party));
            }
            let integer = |name: &str| -> Result<i128, ActionRefusal<H::Error>> {
                let Some(JsonValue::Number(n)) = lot.get(name) else {
                    return Err(ActionRefusal::StockPosting(StockPostError::Purity));
                };
                n.canonical()
                    .parse::<i128>()
                    .map_err(|_| ActionRefusal::StockPosting(StockPostError::Purity))
            };
            let gross = integer("gross_micrograms")?;
            let purity = integer("purity_numerator")?;
            if gross <= 0 || !(1..=24_000_000).contains(&purity) {
                return Err(ActionRefusal::StockPosting(StockPostError::Purity));
            }
            let fine = gross
                .checked_mul(purity)
                .ok_or(ActionRefusal::StockPosting(StockPostError::Overflow))?;
            norn_ir::runtime::Decimal::parse(&fine.to_string())
                .map_err(|_| ActionRefusal::StockPosting(StockPostError::Overflow))?;
            if fine != integer("fine_ticks")? {
                return Err(ActionRefusal::StockPosting(StockPostError::Purity));
            }
        } else if custody {
            return Err(invalid("custody posting requires a metal lot"));
        }
        let accounts = [
            reader.account(&key, &binding.accounts.stock)?,
            reader.account(&key, &binding.accounts.srbnb)?,
            reader.account(&key, &binding.accounts.cogs)?,
        ];
        let uom = text::<H::Error>(&reader.fact(&item)?, "uom")?;
        let masters = [company.clone(), item, warehouse].map(|key| model.canonical(&key));
        let witness =
            partition_key(&masters).map_err(|_encoding| invalid("invalid partition identity"))?;
        let company_values = reader.fact(&company)?;
        let bindings = runtime_bindings(&company_values, &accounts, call)?;
        let binding_pin = norn_ir::canonical::domain_hash_hex(
            b"norn.stock-bindings/1\0",
            &(
                &bindings.templates,
                &bindings.accounts,
                &bindings.currency,
                &bindings.layer,
                &uom,
                &owner,
                is_metal.then_some(&lot),
            ),
        )
        .map_err(|_encoding| invalid("invalid stock binding pin"))?;
        let partition = partitions
            .entry(masters.clone())
            .or_insert(StockPartitionInput {
                masters,
                accounts,
                lines: vec![],
                witness,
                history: vec![],
                bindings,
                binding_pin,
                uom,
                owner,
            });
        partition.lines.push(StockPostLine {
            id: id.clone(),
            quantity: millionths(&values, "quantity")?,
            unit_cost: match binding.operation {
                norn_ir::ActionStockOperation::Receipt => Some(millionths(&values, "rate")?),
                norn_ir::ActionStockOperation::CustodyReceipt => Some(0),
                norn_ir::ActionStockOperation::Delivery
                | norn_ir::ActionStockOperation::CustodyReturn => None,
            },
        });
    }
    for partition in partitions.values_mut() {
        partition.history = reader.history(&partition.witness, &partition.binding_pin)?;
    }
    reader.snapshot.partitions = partitions.into_values().collect();
    Ok(reader.snapshot)
}

fn partition_key(
    masters: &[FactKey; 3],
) -> Result<FactKey, norn_ir::canonical::CanonicalJsonError> {
    norn_ir::canonical::domain_hash_hex(b"norn.stock-partition/1\0", masters)
        .map(|id| FactKey::new("stock.Partition", id))
}

fn natural<X>(values: &JsonObject, property: &str) -> Result<u64, ActionRefusal<X>> {
    let Some(JsonValue::Number(value)) = values.get(property) else {
        return Err(invalid(format!("missing integer {property}")));
    };
    value
        .canonical()
        .parse()
        .map_err(|_parse| invalid(format!("invalid integer {property}")))
}

fn history_event<X>(
    partition: &FactKey,
    key: &FactKey,
    values: &JsonObject,
) -> Result<(u64, FifoMovement), ActionRefusal<X>> {
    if key.object_type.as_str() != "stock.StockEvent"
        || text::<X>(values, "partition_id")? != partition.key
        || text::<X>(values, "id")? != key.key
    {
        return Err(invalid("stock history identity differs from partition"));
    }
    let quantity = millionths(values, "quantity")?;
    let value = millionths(values, "value")?;
    let movement = match text::<X>(values, "operation")?.as_str() {
        "receipt" => FifoMovement::Receipt(FifoLayer {
            receipt: key.key.clone(),
            quantity,
            value,
        }),
        "delivery" => FifoMovement::Delivery(quantity),
        _ => return Err(invalid("invalid stock history movement")),
    };
    Ok((natural(values, "ordinal")?, movement))
}

fn runtime_bindings<X>(
    company: &JsonObject,
    accounts: &[String; 3],
    call: &ActionCall<'_>,
) -> Result<StockPostBindings, ActionRefusal<X>> {
    let account = |id: &String| -> Result<_, ActionRefusal<X>> {
        Ok(norn_ir::posting::AccountingAccountRef {
            scope_key: call
                .scope
                .get("orgId")
                .cloned()
                .ok_or_else(|| invalid("missing organization"))?,
            book_id: text(company, "book_id")?,
            graph_id: text(company, "graph_id")?,
            graph_version: natural(company, "graph_version")?,
            account_id: id.clone(),
        })
    };
    let [stock, srbnb, cogs] = accounts;
    Ok(StockPostBindings {
        accounts: [account(stock)?, account(srbnb)?, account(cogs)?],
        templates: ["receipt".into(), "delivery".into()],
        currency: text(company, "currency")?,
        layer: "posted".into(),
    })
}

/// Lower the authorized snapshot; refusals disclose only authorized identities.
///
/// # Errors
/// Returns typed valuation refusals without writes.
pub fn plan_stock_partition<X>(
    partition: &StockPartitionInput,
    operation: StockPostOperation,
    posting_id: &str,
) -> Result<StockPostPlan, ActionRefusal<X>> {
    history_capacity(
        partition.history.len(),
        partition.lines.len(),
        0,
        ResourceLimitProfile::norn_limits_v1().slice.max_records,
    )?;
    lower_stock_posting(
        &partition.bindings,
        operation,
        posting_id,
        &partition.history,
        &partition.lines,
    )
    .map_err(|error| {
        if error == StockPostError::Fifo(FifoError::InsufficientStock) {
            let requested = partition
                .lines
                .iter()
                .try_fold(0_u128, |sum, line| sum.checked_add(line.quantity));
            let available = replay_fifo(&partition.history).ok().and_then(|layers| {
                layers
                    .iter()
                    .try_fold(0_u128, |sum, layer| sum.checked_add(layer.quantity))
            });
            if let (Some(requested), Some(available)) = (requested, available) {
                let [_, item, warehouse] = &partition.masters;
                return ActionRefusal::InsufficientStock {
                    item: item.key.clone(),
                    warehouse: warehouse.key.clone(),
                    requested: display_quantity(requested),
                    available: display_quantity(available),
                };
            }
        }
        ActionRefusal::StockPosting(error)
    })
}

fn display_quantity(value: u128) -> String {
    let whole = value.checked_div(1_000_000).unwrap_or(0);
    let fraction = value.checked_rem(1_000_000).unwrap_or(0);
    if fraction == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{fraction:06}")
            .trim_end_matches('0')
            .to_owned()
    }
}

/// Complete write proposal; caller-visible data never grants rule authority.
#[derive(Debug)]
pub struct StockEffectPlan {
    /// Voucher transition and computed protected outputs.
    pub output: norn_ir::DecisionOutput,
    /// Closed conserved quantity operation.
    pub quantity: norn_ir::quantity_ledger::QuantityOperation,
    /// Master-derived quantity ledger.
    pub ledger_id: String,
    /// Master unit pinned to the quantity configuration.
    pub unit: String,
    /// New immutable event membership.
    pub links: Vec<crate::cell_host::LinkWrite>,
}

fn stock_invalid<X>(reason: impl Into<String>) -> crate::ActionRefusal<X> {
    crate::ActionRefusal::Decision(crate::DecisionRefusal::InvalidOutput {
        reason: reason.into(),
    })
}
fn stock_json<X, T: serde::Serialize>(
    value: &T,
) -> Result<norn_ir::RuntimeValue, crate::ActionRefusal<X>> {
    norn_ir::runtime::encode(value).map_err(stock_invalid)
}
fn stock_decimal<X>(value: u128) -> Result<norn_ir::RuntimeValue, crate::ActionRefusal<X>> {
    norn_ir::Decimal::parse(&display_quantity(value))
        .map(norn_ir::RuntimeValue::Number)
        .map_err(|_source| stock_invalid("stock amount exceeds exact decimal range"))
}

/// Build the complete stock effect proposal using the shared FIFO lowering.
/// This value is unsealed; only `commit_action` may recompute and seal it.
///
/// # Errors
/// Refuses multiple partitions, backdating, invalid outputs or exhausted capacity.
#[allow(clippy::too_many_lines, reason = "one bounded stock effect proposal")]
pub fn plan_stock_effects<X>(
    snapshot: &StockSnapshot,
    operation: StockPostOperation,
    posting_id: &str,
    mut facts: Vec<norn_ir::FactChange>,
) -> Result<StockEffectPlan, crate::ActionRefusal<X>> {
    use crate::cell_host::{FactVersion, LinkKey, LinkMutation, LinkWrite};
    use crate::stock_fifo::FifoMovement;
    use norn_ir::{FactChange, ObjectTypeName, PropertyId, RuntimeValue as V};
    let [partition] = snapshot.partitions.as_slice() else {
        return Err(stock_invalid(
            "CP1 requires exactly one stock partition per voucher",
        ));
    };
    let Some(FactChange::Transition {
        object_type, key, ..
    }) = facts.first()
    else {
        return Err(stock_invalid("stock rule requires the voucher transition"));
    };
    let target = FactKey::new(object_type.as_str(), key);
    let effective = snapshot
        .facts
        .get(&target)
        .and_then(|s| s.values.as_ref())
        .and_then(|v| v.get("effective_at"))
        .and_then(V::as_str)
        .ok_or_else(|| stock_invalid("missing voucher effective timestamp"))?;
    let effective_key = norn_expr::temporal::timestamp_ordering_key(effective)
        .map_err(|_source| stock_invalid("invalid stock effective timestamp"))?;
    let old = snapshot
        .facts
        .get(&partition.witness)
        .and_then(|s| s.values.as_ref());
    if let Some(prior) = old {
        let prior = prior
            .get("effective_at")
            .and_then(V::as_str)
            .ok_or_else(|| stock_invalid("missing partition effective timestamp"))?;
        if norn_expr::temporal::timestamp_ordering_key(prior)
            .map_err(|_source| stock_invalid("invalid partition timestamp"))?
            > effective_key
        {
            return Err(stock_invalid("CP1 refuses backdated stock posting"));
        }
    }
    let plan = crate::action_commit::stock::plan_stock_partition(partition, operation, posting_id)?;
    let mut history_bytes = snapshot
        .facts
        .iter()
        .filter(|(k, _)| k.object_type.as_str() == "stock.StockEvent")
        .try_fold(0_u64, |sum, (_, state)| {
            let bytes = norn_ir::canonical::canonical_json_of(&state.values)
                .map_err(|_source| stock_invalid("invalid stock history encoding"))?
                .len();
            Ok::<_, crate::ActionRefusal<X>>(
                sum.saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX)),
            )
        })?;
    let mut links = Vec::new();
    for (index, (event, line)) in plan.events.iter().zip(&partition.lines).enumerate() {
        let (quantity, value, operation) = match &event.movement {
            FifoMovement::Receipt(layer) => (layer.quantity, layer.value, "receipt"),
            FifoMovement::Delivery(q) => (
                *q,
                event
                    .allocations
                    .iter()
                    .try_fold(0_u128, |sum, a| sum.checked_add(a.value))
                    .ok_or_else(|| stock_invalid("stock allocation overflow"))?,
                "delivery",
            ),
        };
        let V::Object(values) = norn_ir::runtime_object! (
            "id" => event.id, "partition_id" => partition.witness.key,
            "ordinal" => partition.history.len().saturating_add(index), "operation" => operation,
            "quantity" => stock_decimal(quantity)?, "value" => stock_decimal(value)?,
            "allocations" => stock_json(&event.allocations)?, "owner" => stock_json(partition.owner.as_ref().unwrap_or(&partition.masters[0]))?, "voucher_id" => target.key,
            "line_id" => line.id, "effective_at" => effective,
        ) else {
            return Err(stock_invalid("invalid stock event object"));
        };
        history_bytes = history_bytes.saturating_add(
            u64::try_from(
                norn_ir::canonical::canonical_json_of(&values)
                    .map_err(|_source| stock_invalid("invalid stock event encoding"))?
                    .len(),
            )
            .unwrap_or(u64::MAX),
        );
        let key = FactKey::new("stock.StockEvent", &event.id);
        facts.push(FactChange::Create {
            object_type: key.object_type.clone(),
            key: key.key.clone(),
            values,
        });
        links.push(LinkWrite {
            key: LinkKey::new(partition.witness.clone(), "events", key),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        });
    }
    let (quantity, value) = plan
        .layers
        .iter()
        .try_fold((0_u128, 0_u128), |(q, v), layer| {
            Some((q.checked_add(layer.quantity)?, v.checked_add(layer.value)?))
        })
        .ok_or_else(|| stock_invalid("stock partition total overflow"))?;
    let V::Object(values) = norn_ir::runtime_object! (
        "id" => partition.witness.key, "event_count" => partition.history.len().saturating_add(plan.events.len()),
        "binding_pin" => partition.binding_pin, "owner" => stock_json(partition.owner.as_ref().unwrap_or(&partition.masters[0]))?, "quantity" => stock_decimal(quantity)?, "value" => stock_decimal(value)?,
        "layers" => stock_json(&plan.layers)?, "effective_at" => effective,
    ) else {
        return Err(stock_invalid("invalid stock witness object"));
    };
    history_bytes = history_bytes.saturating_add(
        u64::try_from(
            norn_ir::canonical::canonical_json_of(&values)
                .map_err(|_source| stock_invalid("invalid stock witness encoding"))?
                .len(),
        )
        .unwrap_or(u64::MAX),
    );
    if history_bytes > 64 * 1024 {
        return Err(stock_invalid(
            "stock history byte capacity exceeded; posting refused",
        ));
    }
    if old.is_some() {
        facts.extend(values.into_iter().map(|(property, value)| FactChange::Set {
            object_type: partition.witness.object_type.clone(),
            key: partition.witness.key.clone(),
            property: PropertyId(property),
            value,
        }));
    } else {
        facts.push(FactChange::Create {
            object_type: partition.witness.object_type.clone(),
            key: partition.witness.key.clone(),
            values,
        });
    }
    for leg in plan.gl.iter().filter(|_| partition.owner.is_none()) {
        let V::Object(values) = norn_ir::runtime_object! ( "id" => leg.leg_id, "journal_id" => posting_id, "leg" => stock_json(leg)? )
        else {
            return Err(stock_invalid("invalid GL object"));
        };
        facts.push(FactChange::Create {
            object_type: ObjectTypeName("stock.GLLeg".into()),
            key: leg.leg_id.clone(),
            values,
        });
    }
    Ok(StockEffectPlan {
        output: norn_ir::DecisionOutput {
            facts,
            commands: vec![],
            timers: vec![],
        },
        quantity: plan.quantity,
        ledger_id: format!("stock/{}", partition.witness.key),
        unit: partition.uom.clone(),
        links,
    })
}

pub(super) type PreparedStockCommit = (
    crate::PreparedDecision,
    Vec<crate::cell_host::LinkWrite>,
    (norn_ir::quantity_ledger::QuantityRequest, String),
);

#[allow(
    clippy::too_many_arguments,
    reason = "one sealed stock transaction preparation"
)]
pub(super) fn prepare_stock_commit<H: CellHost + ?Sized>(
    host: &H,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    snapshot: &StockSnapshot,
    slice: &mut FactSlice,
    links: &mut Vec<LinkReadExpectation>,
    output: &norn_ir::DecisionOutput,
    limits: &crate::EnvelopeLimits,
) -> Result<PreparedStockCommit, ActionRefusal<H::Error>> {
    use norn_ir::quantity_ledger::{
        QUANTITY_CELL_VERSION, QuantityBalanceVersion, QuantityRequest,
    };
    let rule = catalog.checked_stock_rule::<H::Error>(call)?;
    for (key, state) in &snapshot.facts {
        super::reconcile_action_target_snapshot::<H::Error>(catalog, slice, key, state)?;
    }
    for link in &snapshot.links {
        super::record_action_link_read(links, &link.source, &link.link, link.targets.clone())?;
    }
    let operation =
        match call
            .action
            .stock_post
            .as_ref()
            .ok_or_else(|| invalid("missing stock rule"))?
            .operation
        {
            norn_ir::ActionStockOperation::Receipt
            | norn_ir::ActionStockOperation::CustodyReceipt => StockPostOperation::Receipt,
            norn_ir::ActionStockOperation::Delivery
            | norn_ir::ActionStockOperation::CustodyReturn => StockPostOperation::Delivery,
        };
    let posting_id = norn_ir::canonical::domain_hash_hex(
        b"stock.posting/1\0",
        &(&call.action.action_id, call.idempotency_key.as_bytes()),
    )
    .map_err(|_error| invalid("invalid stock posting identity"))?;
    let mut plan = plan_stock_effects(snapshot, operation, &posting_id, output.facts.clone())?;
    plan.output.commands.clone_from(&output.commands);
    let mut request = QuantityRequest {
        version: QUANTITY_CELL_VERSION.into(),
        ledger_id: plan.ledger_id,
        balance_versions: vec![],
        operation: plan.quantity,
    };
    let state = host
        .load_quantity(&request)
        .map_err(host_failure)?
        .ok_or_else(|| invalid("stock quantity reads unavailable"))?;
    let pack = crate::stock_posting::stock_quantity_pack(&plan.unit)
        .map_err(|_error| invalid("invalid stock unit"))?;
    if state.pack.as_ref() != Some(&pack) || state.unrestricted_accounts != ["external-source"] {
        return Err(invalid(
            "stock quantity configuration differs from admitted pack",
        ));
    }
    request.balance_versions = crate::quantity::quantity_balance_keys(&request, &state)
        .map_err(|error| ActionRefusal::Decision(crate::DecisionRefusal::Quantity(error)))?
        .into_iter()
        .map(|key| QuantityBalanceVersion {
            version: state.balances.get(&key).map_or(0, |b| b.version),
            account: key.account,
            unit: key.unit,
            layer: key.layer,
        })
        .collect();
    let prepared = crate::rule_authority::prepare_rule_decision_with_limits(
        catalog,
        slice,
        &call.turn,
        &rule.seal(plan.output),
        &BTreeSet::new(),
        limits,
    )?;
    Ok((prepared, plan.links, (request, posting_id)))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::too_many_lines,
        reason = "conformance fixture assembly"
    )]
    use super::*;
    use serde_json::json;
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "stock proposal and capacity conformance"
    )]
    fn post_partition_refusal_names_authoritative_stock() {
        let accounts =
            ["stock", "srbnb", "cogs"].map(|id| norn_ir::posting::AccountingAccountRef {
                scope_key: "demo".into(),
                book_id: "book".into(),
                graph_id: "graph".into(),
                graph_version: 1,
                account_id: id.into(),
            });
        let mut partition = StockPartitionInput {
            masters: [
                FactKey::new("stock.Company", "c"),
                FactKey::new("stock.Item", "i"),
                FactKey::new("stock.Warehouse", "w"),
            ],
            accounts: ["stock".into(), "srbnb".into(), "cogs".into()],
            witness: FactKey::new("stock.Partition", "p"),
            binding_pin: "pin".into(),
            owner: None,
            uom: "ea".into(),
            history: vec![FifoMovement::Receipt(FifoLayer {
                receipt: "prior".into(),
                quantity: 5_000_000,
                value: 50_000_000,
            })],
            lines: vec![StockPostLine {
                id: "line".into(),
                quantity: 6_250_000,
                unit_cost: None,
            }],
            bindings: StockPostBindings {
                templates: ["receipt".into(), "delivery".into()],
                accounts,
                currency: "USD".into(),
                layer: "posted".into(),
            },
        };
        let error = plan_stock_partition::<std::convert::Infallible>(
            &partition,
            StockPostOperation::Delivery,
            "dn",
        )
        .unwrap_err();
        assert_eq!(error.code(), "stock.insufficient_stock");
        assert_eq!(
            error.to_string(),
            "insufficient stock for item i in warehouse w: requested 6.25, available 5"
        );

        partition.lines[0].quantity = 4_000_000;
        let target = FactKey::new("stock.DeliveryNote", "dn");
        let mut snapshot = StockSnapshot {
            facts: FactSlice::from([
                (
                    target.clone(),
                    super::super::FactState {
                        version: crate::FactVersion(1),
                        values: Some(
                            serde_json::from_value(json!({"effective_at":"2026-11-01T01:00:00Z"}))
                                .unwrap(),
                        ),
                    },
                ),
                (
                    partition.witness.clone(),
                    super::super::FactState {
                        version: crate::FactVersion(1),
                        values: Some(
                            serde_json::from_value(json!({"effective_at":"2026-11-01T00:00:00Z"}))
                                .unwrap(),
                        ),
                    },
                ),
                (
                    FactKey::new("stock.StockEvent", "prior"),
                    super::super::FactState {
                        version: crate::FactVersion(1),
                        values: Some(serde_json::from_value(json!({"allocations":[]})).unwrap()),
                    },
                ),
            ]),
            partitions: vec![partition],
            ..StockSnapshot::default()
        };
        let transition = || {
            vec![norn_ir::FactChange::Transition {
                object_type: target.object_type.clone(),
                key: target.key.clone(),
                lifecycle: norn_ir::PropertyId("status".into()),
                transition: norn_ir::TransitionId("submit".into()),
            }]
        };
        let effects = plan_stock_effects::<std::convert::Infallible>(
            &snapshot,
            StockPostOperation::Delivery,
            "dn",
            transition(),
        )
        .unwrap();
        assert_eq!(effects.links.len(), 1, "POST-EFFECTS event membership");
        let gl: Vec<norn_ir::posting::AccountingLegIr> = effects
            .output
            .facts
            .iter()
            .filter_map(|fact| match fact {
                norn_ir::FactChange::Create {
                    object_type,
                    values,
                    ..
                } if object_type.as_str() == "stock.GLLeg" => {
                    Some(norn_ir::runtime::decode(values.get("leg").unwrap()).unwrap())
                }
                norn_ir::FactChange::Create { .. }
                | norn_ir::FactChange::Set { .. }
                | norn_ir::FactChange::Delete { .. }
                | norn_ir::FactChange::Transition { .. } => None,
            })
            .collect();
        assert_eq!(
            gl.iter()
                .map(|leg| leg.amount_unscaled.as_str())
                .collect::<Vec<_>>(),
            ["4000", "-4000"],
            "POST-EFFECTS balanced derived GL"
        );
        assert!(effects.output.facts.iter().any(|fact| matches!(fact, norn_ir::FactChange::Set { property, value, .. } if property.as_str() == "quantity" && matches!(value, JsonValue::Number(number) if number.canonical() == "1"))), "POST-EFFECTS remaining quantity");
        snapshot
            .facts
            .get_mut(&target)
            .unwrap()
            .values
            .as_mut()
            .unwrap()
            .insert(
                "effective_at".into(),
                JsonValue::String("2026-10-31T00:00:00Z".into()),
            );
        assert!(
            plan_stock_effects::<std::convert::Infallible>(
                &snapshot,
                StockPostOperation::Delivery,
                "dn",
                transition()
            )
            .unwrap_err()
            .to_string()
            .contains("backdated"),
            "POST-NO-BACKDATE"
        );
        snapshot
            .facts
            .get_mut(&target)
            .unwrap()
            .values
            .as_mut()
            .unwrap()
            .insert(
                "effective_at".into(),
                JsonValue::String("2026-11-01T01:00:00Z".into()),
            );
        snapshot
            .facts
            .get_mut(&FactKey::new("stock.StockEvent", "prior"))
            .unwrap()
            .values
            .as_mut()
            .unwrap()
            .insert(
                "allocations".into(),
                JsonValue::String("x".repeat(64 * 1024)),
            );
        assert!(
            plan_stock_effects::<std::convert::Infallible>(
                &snapshot,
                StockPostOperation::Delivery,
                "dn",
                transition()
            )
            .unwrap_err()
            .to_string()
            .contains("byte capacity"),
            "POST-HISTORY-BYTES"
        );
        partition = snapshot.partitions.pop().unwrap();
        partition.history = (0..255)
            .map(|i| {
                FifoMovement::Receipt(FifoLayer {
                    receipt: i.to_string(),
                    quantity: 1_000_000,
                    value: 1_000_000,
                })
            })
            .collect();
        partition.lines[0].quantity = 1_000_000;
        assert!(
            plan_stock_partition::<std::convert::Infallible>(
                &partition,
                StockPostOperation::Delivery,
                "last"
            )
            .is_ok(),
            "POST-HISTORY-CAPACITY 255 + 1"
        );
        partition.history.push(FifoMovement::Delivery(1_000_000));
        assert!(
            matches!(
                plan_stock_partition::<std::convert::Infallible>(
                    &partition,
                    StockPostOperation::Delivery,
                    "over"
                ),
                Err(ActionRefusal::SliceUnavailable { .. })
            ),
            "POST-HISTORY-CAPACITY 256 + 1"
        );
        let records = ResourceLimitProfile::norn_limits_v1().slice.max_records;
        let room = records.saturating_sub(u32::try_from(MAX_STOCK_HISTORY_EVENTS).unwrap());
        assert!(history_capacity::<std::convert::Infallible>(256, 0, room, records).is_ok());
        assert!(
            history_capacity::<std::convert::Infallible>(256, 0, room.saturating_add(1), records)
                .is_err()
        );
    }
    #[test]
    fn post_history_identity_and_exact_shape() {
        let partition = FactKey::new("stock.Partition", "p");
        let event = FactKey::new("stock.StockEvent", "e");
        let values: JsonObject = serde_json::from_value(json!({"id":"e", "partition_id":"p", "ordinal":0, "operation":"receipt", "quantity":5, "value":50})).unwrap();
        assert_eq!(
            history_event::<std::convert::Infallible>(&partition, &event, &values).unwrap(),
            (
                0,
                FifoMovement::Receipt(FifoLayer {
                    receipt: "e".into(),
                    quantity: 5_000_000,
                    value: 50_000_000
                })
            )
        );
        for field in ["id", "partition_id", "ordinal", "operation", "quantity"] {
            let mut malformed = values.clone();
            malformed.insert(field.into(), JsonValue::String("invalid".into()));
            assert!(
                history_event::<std::convert::Infallible>(&partition, &event, &malformed).is_err()
            );
        }
    }
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
