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

/// One master-selected partition, with lines in owned-manifest order.
#[derive(Debug)]
pub struct StockPartitionInput {
    /// Authoritative company, item and warehouse identities.
    pub masters: [FactKey; 3],
    /// Quantity and acquisition cost read from owned lines, in millionths.
    pub lines: Vec<StockPostLine>,
    /// Fenced partition identity, including an absent first-post witness.
    pub witness: FactKey,
    /// Prior movements in dense partition order.
    pub history: Vec<FifoMovement>,
    /// Master-selected closed runtime bindings.
    pub bindings: StockPostBindings,
    /// Immutable posting configuration/UOM pin checked against prior history.
    pub binding_pin: String,
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
        let keys = self.targets(partition, "events")?;
        let count = keys.len();
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
        let accounts = [
            reader.account(&key, &binding.accounts.stock)?,
            reader.account(&key, &binding.accounts.srbnb)?,
            reader.account(&key, &binding.accounts.cogs)?,
        ];
        let masters = [company.clone(), item, warehouse];
        let witness =
            partition_key(&masters).map_err(|_encoding| invalid("invalid partition identity"))?;
        let company_values = reader.fact(&company)?;
        let bindings = runtime_bindings(&company_values, &accounts, call)?;
        let uom = text::<H::Error>(&reader.fact(&masters[1])?, "uom")?;
        let binding_pin = norn_ir::canonical::domain_hash_hex(
            b"norn.stock-bindings/1\0",
            &(
                &bindings.templates,
                &bindings.accounts,
                &bindings.currency,
                &bindings.layer,
                uom,
            ),
        )
        .map_err(|_encoding| invalid("invalid stock binding pin"))?;
        let partition = partitions
            .entry(masters.clone())
            .or_insert(StockPartitionInput {
                masters,
                lines: vec![],
                witness,
                history: vec![],
                bindings,
                binding_pin,
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
pub(super) fn plan_stock_partition<X>(
    partition: &StockPartitionInput,
    operation: StockPostOperation,
    posting_id: &str,
) -> Result<StockPostPlan, ActionRefusal<X>> {
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        reason = "conformance fixtures"
    )]
    use super::*;
    use crate::{
        ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
        CellCommit, FactEvent, FactMutation, FactVersion, FactWrite, LinkKey, LinkMutation,
        LinkWrite,
    };
    use serde_json::json;
    struct Allow;
    impl ActionAuthorizer for Allow {
        fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
            Ok(())
        }
        fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
            Ok(())
        }
        fn authorize_read_type(
            &self,
            _: &ActionReadTypeAuthzRequest<'_>,
        ) -> Result<(), ActionDenial> {
            Ok(())
        }
    }
    #[test]
    fn post_history_fences_and_refusal() {
        let artifacts: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../../../tools/norn-ts/fixtures/stock/stock.artifacts.json"
        ))
        .unwrap();
        let types: Vec<norn_ir::ObjectTypeDef> = artifacts
            .iter()
            .filter_map(|a| a["payload"].get("objectType"))
            .map(|v| serde_json::from_value(v.clone()).unwrap())
            .collect();
        let action: norn_ir::ActionRoot = serde_json::from_value(
            artifacts
                .iter()
                .find(|a| a["artifactId"] == "stock.receipt.submit")
                .unwrap()["payload"]
                .clone(),
        )
        .unwrap();
        let catalog = TypeCatalog::new(&types);
        let mut cell =
            norn_testkit::cell_host::MemoryCell::<FactEvent, JsonObject, JsonObject>::new(|_| {
                JsonObject::new()
            })
            .with_types(types.clone());
        let masters = [
            FactKey::new("stock.Company", "c"),
            FactKey::new("stock.Item", "i"),
            FactKey::new("stock.Warehouse", "w"),
        ];
        let partition = partition_key(&masters).unwrap();
        let call = ActionCall {
            action: &action,
            action_artifact_id: "stock.receipt.submit",
            action_version: "0.1.0",
            scope: BTreeMap::from([
                ("orgId".into(), "org_demo".into()),
                ("environmentId".into(), "dev".into()),
            ]),
            actor: "test".into(),
            input: serde_json::from_value(json!({"id":"pr"})).unwrap(),
            idempotency_key: "submit".into(),
            object_type: None,
            turn: crate::DecisionTurn {
                id: "turn".into(),
                now: crate::LogicalTime::from_millis(1),
                world: crate::World::Main,
                consumes: None,
            },
        };
        let snapshot = |cell: &_| {
            let limits = ResourceLimitProfile::norn_limits_v1().slice;
            let mut reader = Reader {
                context: ActionSliceContext {
                    host: cell,
                    catalog: &catalog,
                    call: &call,
                    effective_input: &call.input,
                    authorizer: &Allow,
                },
                snapshot: StockSnapshot::default(),
                meter: SliceMeter::new(limits),
                observed: 0,
                types: BTreeSet::new(),
                limits,
            };
            let history = reader.history(&partition, "pin").unwrap();
            (reader.snapshot, history)
        };
        let (empty, history) = snapshot(&cell);
        assert!(history.is_empty());
        assert_eq!(
            empty.facts[&partition].version,
            FactVersion::ABSENT,
            "POST-HISTORY-FENCE absence"
        );
        assert!(empty.links[0].targets.is_empty());
        let event = FactKey::new("stock.StockEvent", "e");
        let write = |key, values| FactWrite {
            key,
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(values).unwrap(),
            },
        };
        cell.commit(CellCommit { writes: vec![write(partition.clone(), json!({"id":partition.key,"event_count":1,"binding_pin":"pin"})), write(event.clone(), json!({"id":"e","partition_id":partition.key,"ordinal":0,"operation":"receipt","quantity":5,"value":50}))],
            links: vec![LinkWrite { key: LinkKey::new(partition.clone(), "events", event.clone()), expected: FactVersion::ABSENT, mutation: LinkMutation::Add { values: JsonObject::new() } }], ..CellCommit::empty() }).unwrap();
        assert!(
            cell.commit(CellCommit::<FactEvent, JsonObject, JsonObject> {
                link_reads: empty.links.clone(),
                ..CellCommit::empty()
            })
            .is_err(),
            "POST-HISTORY-FENCE empty adjacency race"
        );
        let stale = CellCommit::<FactEvent, JsonObject, JsonObject> {
            reads: vec![super::super::FactExpectation {
                key: partition.clone(),
                expected: FactVersion::ABSENT,
            }],
            link_reads: empty.links,
            ..CellCommit::empty()
        };
        assert!(
            cell.commit(stale).is_err(),
            "POST-HISTORY-FENCE first-post race"
        );
        let (read, history) = snapshot(&cell);
        assert!(read.facts.contains_key(&event));
        let company = serde_json::from_value(
            json!({"book_id":"b","graph_id":"g","graph_version":1,"currency":"USD"}),
        )
        .unwrap();
        let accounts = ["stock".into(), "srbnb".into(), "cogs".into()];
        let bindings =
            runtime_bindings::<std::convert::Infallible>(&company, &accounts, &call).unwrap();
        let p = StockPartitionInput {
            masters,
            binding_pin: "pin".into(),
            witness: partition,
            history,
            bindings,
            lines: vec![StockPostLine {
                id: "l".into(),
                quantity: 6_000_000,
                unit_cost: None,
            }],
        };
        let before = cell.ledger().len();
        let error = plan_stock_partition::<std::convert::Infallible>(
            &p,
            StockPostOperation::Delivery,
            "dn",
        )
        .unwrap_err();
        assert_eq!(error.code(), "stock.insufficient_stock");
        assert_eq!(
            error.to_string(),
            "insufficient stock for item i in warehouse w: requested 6, available 5"
        );
        assert_eq!(cell.ledger().len(), before, "POST-REFUSAL-REASON no writes");
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
