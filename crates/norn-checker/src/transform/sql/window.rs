//! Window lowering: one `window` node over the columns a SELECT names.
//! Only what the window operator keeps is lowered; a modifier it has no
//! field for (`DISTINCT`, `*`) refuses rather than being dropped.

use serde_json::json;

use super::{Ex, Lowering, Over, Refusal, Scope, refusal};

impl Lowering<'_> {
    /// One window node over output columns named by the select list.
    ///
    /// A window reads a column of its input: the one a select item names, or
    /// one that `carried` shows holds the same value (ZEN over the source
    /// row: a projected column, a group key, a source id). A column is never
    /// read by its bare name, which may hold another value
    /// (`c.region AS email` is not `c.email`).
    pub(super) fn window(
        &mut self,
        input: &str,
        windows: &[(String, Ex, usize)],
        names: &[(Ex, String)],
        scope: &Scope,
        carried: &[(String, String)],
    ) -> Result<String, Refusal> {
        let column_of = |ex: &Ex, at: usize| -> Result<String, Refusal> {
            if let Some((_, name)) = names.iter().find(|(selected, _)| selected == ex) {
                return Ok(name.clone());
            }
            let value = scope.zen(ex, at).ok();
            carried
                .iter()
                .find(|(carries, _)| value.as_ref() == Some(carries))
                .map(|(_, name)| name.clone())
                .ok_or_else(|| refusal("a window reads a selected column or a source id", at))
        };
        let mut over: Option<&Over> = None;
        let mut columns = Vec::new();
        for (name, ex, at) in windows {
            let Ex::Call(call) = ex else {
                return Err(refusal("expected a window function", *at));
            };
            let Some(this) = &call.over else {
                return Err(refusal("expected OVER", *at));
            };
            if over.is_some_and(|first| first != this) {
                return Err(refusal("windows in one SELECT share one OVER (…)", *at));
            }
            over = Some(this);
            // The window operator has no field for either: lowering past one
            // would run `SUM(DISTINCT x)` as `SUM(x)`.
            if call.distinct || call.star {
                let modifier = if call.distinct { "DISTINCT …" } else { "*" };
                return Err(refusal(
                    format!(
                        "{}({modifier}) OVER (…) has no typed window operator",
                        call.name.to_uppercase()
                    ),
                    *at,
                ));
            }
            let function = match (call.name.as_str(), call.args.as_slice()) {
                ("row_number" | "rank" | "dense_rank", []) => {
                    json!({"name": name, "fn": call.name})
                }
                ("lag" | "lead", [value]) => {
                    json!({"name": name, "fn": call.name, "column": column_of(value, *at)?})
                }
                ("lag" | "lead", [value, Ex::Number(offset)]) => {
                    json!({"name": name, "fn": call.name, "column": column_of(value, *at)?, "offset": offset.parse::<u32>().map_err(|_error| refusal("an offset is a whole number", *at))?})
                }
                ("sum", [value]) => {
                    json!({"name": name, "fn": "running_sum", "column": column_of(value, *at)?})
                }
                _ => {
                    return Err(refusal(
                        format!(
                            "{} OVER (…) has no typed window operator",
                            call.name.to_uppercase()
                        ),
                        *at,
                    ));
                }
            };
            columns.push(function);
        }
        let over = over.ok_or_else(|| refusal("expected a window", 0))?;
        let partition = over
            .partition
            .iter()
            .map(|ex| column_of(ex, 0))
            .collect::<Result<Vec<_>, _>>()?;
        let order = over
            .order
            .iter()
            .map(|(ex, descending)| Ok(json!({"column": column_of(ex, 0)?, "direction": if *descending { "desc" } else { "asc" }})))
            .collect::<Result<Vec<_>, Refusal>>()?;
        let id = self.fresh("windowed");
        self.nodes.insert(
            id.clone(),
            json!({"op": "window", "input": input, "partitionBy": partition, "orderBy": order, "columns": columns}),
        );
        Ok(id)
    }
}
