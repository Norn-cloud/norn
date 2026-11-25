//! VIEW-ENGINE-1 on `DataFusion`: each node's rows, materialized once and
//! bounded, as the next nodes' input. A node's rows carry a pointer back to
//! the rows they derive from (one input row, a left and a right row, a
//! branch and its row, or a group's rows); its lineage, the scanned input
//! rows each row derives from, is resolved from those pointers here, in
//! exact sorted sets, and its rows are renumbered for the nodes after it.

use std::sync::Arc;

use datafusion::arrow::array::{Array, ArrayRef, ListArray, UInt64Array};
use datafusion::arrow::datatypes::{Field, Schema, SchemaRef};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::datasource::MemTable;
use datafusion::prelude::{DataFrame, SessionContext};

use super::kinds::{ROW, pointers};
use super::lower::{BRANCH, LEFT, MEMBERS, RIGHT};
use crate::CellError;

/// The scanned input rows one row derives from: indexes into the
/// evaluation's member list, sorted and distinct.
pub(super) type Lineage = Arc<[u32]>;

/// One node's materialized rows.
pub(super) struct Table {
    pub schema: SchemaRef,
    pub batches: Vec<RecordBatch>,
    pub lineage: Vec<Lineage>,
}

/// Where a node's rows point back to.
pub(super) enum Back<'t> {
    /// One input row each (`__r`).
    One(&'t Table),
    /// A left row and, unless unmatched, a right row (`__ra`, `__rb`).
    Two(&'t Table, &'t Table),
    /// A branch (`__u`) and that branch's row (`__r`).
    Branch(Vec<&'t Table>),
    /// A group's rows (`__rs`).
    Many(&'t Table),
}

pub(super) fn engine<E: std::fmt::Display>(error: E) -> CellError {
    CellError::Engine(error.to_string())
}

impl Table {
    /// This table as a `DataFrame` of one partition.
    pub(super) fn frame(&self, ctx: &SessionContext) -> Result<DataFrame, CellError> {
        let memory = MemTable::try_new(Arc::clone(&self.schema), vec![self.batches.clone()])
            .map_err(engine)?;
        ctx.read_table(Arc::new(memory)).map_err(engine)
    }

    /// A table of `columns` (each of the same rows) and their lineage.
    pub(super) fn of(
        fields: Vec<Field>,
        mut columns: Vec<ArrayRef>,
        lineage: Vec<Lineage>,
    ) -> Result<Self, CellError> {
        let mut fields = fields;
        fields.push(Field::new(
            ROW,
            datafusion::arrow::datatypes::DataType::UInt64,
            false,
        ));
        columns.push(pointers(lineage.len()));
        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new(Arc::clone(&schema), columns).map_err(engine)?;
        Ok(Self {
            schema,
            batches: vec![batch],
            lineage,
        })
    }

    /// The rows.
    pub(super) fn rows(&self) -> usize {
        self.lineage.len()
    }
}

fn is_pointer(name: &str) -> bool {
    [ROW, LEFT, RIGHT, BRANCH, MEMBERS].contains(&name)
}

fn u64_column<'b>(batch: &'b RecordBatch, name: &str) -> Result<&'b UInt64Array, CellError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
        .ok_or_else(|| CellError::Engine(format!("a node lost its pointer {name}")))
}

fn at(lineage: &[Lineage], pointer: u64) -> Result<&Lineage, CellError> {
    usize::try_from(pointer)
        .ok()
        .and_then(|index| lineage.get(index))
        .ok_or_else(|| CellError::Engine("a pointer past its input".to_owned()))
}

/// Two sorted lineages merged.
fn merged(left: &[u32], right: &[u32]) -> Lineage {
    let mut out: Vec<u32> = left.iter().chain(right).copied().collect();
    out.sort_unstable();
    out.dedup();
    out.into()
}

/// The lineage of row `row` of `batch`, from its pointers into `back`.
fn lineage_of(batch: &RecordBatch, row: usize, back: &Back<'_>) -> Result<Lineage, CellError> {
    Ok(match back {
        Back::One(input) => Arc::clone(at(&input.lineage, u64_column(batch, ROW)?.value(row))?),
        Back::Two(left, right) => {
            let lefts = u64_column(batch, LEFT)?;
            let rights = u64_column(batch, RIGHT)?;
            let from_left = at(&left.lineage, lefts.value(row))?;
            if rights.is_null(row) {
                Arc::clone(from_left)
            } else {
                merged(from_left, at(&right.lineage, rights.value(row))?)
            }
        }
        Back::Branch(inputs) => {
            let branch = usize::try_from(u64_column(batch, BRANCH)?.value(row))
                .ok()
                .and_then(|branch| inputs.get(branch))
                .ok_or_else(|| CellError::Engine("a branch past its inputs".to_owned()))?;
            Arc::clone(at(&branch.lineage, u64_column(batch, ROW)?.value(row))?)
        }
        Back::Many(input) => {
            let lists = batch
                .column_by_name(MEMBERS)
                .and_then(|column| column.as_any().downcast_ref::<ListArray>())
                .ok_or_else(|| CellError::Engine("a group lost its rows".to_owned()))?;
            if lists.is_null(row) {
                return Ok(Arc::from(Vec::new()));
            }
            let rows = lists.value(row);
            let rows = rows
                .as_any()
                .downcast_ref::<UInt64Array>()
                .ok_or_else(|| CellError::Engine("a group's rows are not pointers".to_owned()))?;
            let mut out = Vec::new();
            for pointer in rows.iter().flatten() {
                out.extend_from_slice(at(&input.lineage, pointer)?);
            }
            out.sort_unstable();
            out.dedup();
            out.into()
        }
    })
}

/// A node's collected rows (of `schema`) as a table: lineage resolved from
/// the pointers into `back`, pointers dropped, rows renumbered.
pub(super) fn materialize(
    schema: &Schema,
    batches: &[RecordBatch],
    back: &Back<'_>,
) -> Result<Table, CellError> {
    // The collected batches' own schema (its nullability is what they hold);
    // the plan's when nothing was collected.
    let schema = batches
        .first()
        .map_or_else(|| Arc::new(schema.clone()), RecordBatch::schema);
    let kept: Vec<usize> = schema
        .fields()
        .iter()
        .enumerate()
        .filter(|(_, field)| !is_pointer(field.name()))
        .map(|(index, _)| index)
        .collect();
    let mut fields: Vec<Field> = kept
        .iter()
        .filter_map(|index| schema.fields().get(*index))
        .map(|field| field.as_ref().clone())
        .collect();
    fields.push(Field::new(
        ROW,
        datafusion::arrow::datatypes::DataType::UInt64,
        false,
    ));
    let out_schema = Arc::new(Schema::new(fields));
    let mut lineage = Vec::new();
    let mut out = Vec::with_capacity(batches.len());
    for batch in batches {
        let first = lineage.len();
        for row in 0..batch.num_rows() {
            lineage.push(lineage_of(batch, row, back)?);
        }
        let mut columns: Vec<ArrayRef> = kept
            .iter()
            .filter_map(|index| batch.columns().get(*index).cloned())
            .collect();
        columns.push(Arc::new(UInt64Array::from_iter_values(
            (first..lineage.len()).map(|row| u64::try_from(row).unwrap_or(u64::MAX)),
        )));
        out.push(RecordBatch::try_new(Arc::clone(&out_schema), columns).map_err(engine)?);
    }
    Ok(Table {
        schema: out_schema,
        batches: out,
        lineage,
    })
}

#[cfg(test)]
mod tests {
    use super::merged;

    #[test]
    fn lineages_merge_sorted_and_distinct() {
        assert_eq!(&*merged(&[1, 4, 9], &[2, 4, 10]), &[1, 2, 4, 9, 10]);
        assert_eq!(&*merged(&[], &[3]), &[3]);
    }
}
