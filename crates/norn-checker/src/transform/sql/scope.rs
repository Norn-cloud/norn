//! Source columns of a SELECT's flat row: which column a name reads, and
//! the ZEN of a row expression over them.

use super::{Ex, Refusal, Scope, refusal, zen_column, zen_number, zen_text};

impl Scope {
    /// The flat-row name of `alias.column` (the base source's alias when
    /// `alias` is `None`).
    pub(super) fn column(
        &self,
        alias: Option<&str>,
        column: &str,
        at: usize,
    ) -> Result<String, Refusal> {
        let alias = alias.unwrap_or(&self.base);
        let (_, prefix) = self
            .aliases
            .get(alias)
            .ok_or_else(|| refusal(format!("no source is named {alias:?}"), at))?;
        let name = format!("{prefix}{column}");
        // The joined row is flat: a column is read only when its source has
        // it and no other source's column flattens to the same name, so
        // neither `o.c_region` nor `placed.at` reads another source's column.
        let has = |source: &str, column: &str| {
            self.columns
                .get(source)
                .is_some_and(|columns| columns.contains(column))
        };
        let shared = self.aliases.iter().any(|(other, (_, other_prefix))| {
            other != alias
                && name
                    .strip_prefix(other_prefix.as_str())
                    .is_some_and(|rest| has(other, rest))
        });
        if self.aliases.len() > 1 && !has(alias, column) {
            return Err(refusal(format!("{alias} has no column {column:?}"), at));
        }
        if self.aliases.len() > 1 && shared {
            return Err(refusal(
                format!(
                    "{alias}.{column} and another source's column are both {name:?} in the joined row"
                ),
                at,
            ));
        }
        Ok(name)
    }

    /// ZEN over `input.<column>` for a row expression; aggregates and windows refuse.
    pub(super) fn zen(&self, ex: &Ex, at: usize) -> Result<String, Refusal> {
        Ok(match ex {
            Ex::Column(alias, column) => {
                zen_column(&self.column(alias.as_deref(), column, at)?, at)?
            }
            Ex::Number(number) => zen_number(number),
            Ex::Text(text) => zen_text(text, at)?,
            Ex::Bool(flag) => flag.to_string(),
            Ex::Null => "null".to_owned(),
            // Every compound form is wrapped whole: ZEN binds `not` looser
            // than `??`, so an unwrapped `not (a) ?? b` would read as
            // `not (a ?? b)`.
            Ex::Not(inner) => format!("(not ({}))", self.zen(inner, at)?),
            Ex::Negate(inner) => format!("(-({}))", self.zen(inner, at)?),
            Ex::Binary(op, left, right) => {
                format!("({} {op} {})", self.zen(left, at)?, self.zen(right, at)?)
            }
            Ex::IsNull(inner, negated) => format!(
                "({} {} null)",
                self.zen(inner, at)?,
                if *negated { "!=" } else { "==" }
            ),
            Ex::Case(condition, then, otherwise) => format!(
                "({} ? {} : {})",
                self.zen(condition, at)?,
                self.zen(then, at)?,
                self.zen(otherwise, at)?
            ),
            Ex::Coalesce(args) => {
                if args.len() < 2 {
                    return Err(refusal("COALESCE takes two or more values", at));
                }
                // Nested left to right, so the tree does not rest on how ZEN
                // associates `??`.
                let mut parts = args.iter().map(|arg| self.zen(arg, at));
                let first = parts
                    .next()
                    .ok_or_else(|| refusal("COALESCE takes two or more values", at))??;
                parts.try_fold(
                    first,
                    |left, right: Result<String, Refusal>| -> Result<String, Refusal> {
                        Ok(format!("({left} ?? {})", right?))
                    },
                )?
            }
            Ex::Call(call) => {
                return Err(refusal(
                    format!("{} is not a row expression here", call.name.to_uppercase()),
                    call.at,
                ));
            }
        })
    }
}
