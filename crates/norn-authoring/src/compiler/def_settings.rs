//! Governed setting (`Norn.setting`), exact decimal literal validation, and
//! strict RFC 3339 timestamp parsing split out of [`super::def_models`].

use std::collections::HashSet;

use serde_json::{Map, Value};
use tsrs_ast::{Kind, Node};
use tsrs_checker::Checker;
use tsrs_core::P;

use super::def_helpers::{DefinitionCompiler, LoweredDef, Members};
use super::def_literals::is_unsigned_decimal_text;
use super::types::{CompileError, EnvelopeMeta, EnvelopeOptions, build_envelope};
use super::value_schema::ExpansionBudget;

const MAX_DECIMAL_MANTISSA: u128 = (1_u128 << 96).saturating_sub(1);
const MAX_DECIMAL_SCALE: usize = 28;

impl DefinitionCompiler<'_> {
    pub(crate) fn compile_setting(
        &mut self,
        checker: &mut Checker,
        expansion: &mut ExpansionBudget,
        call: P<Node>,
        options: &EnvelopeOptions,
    ) -> Result<LoweredDef, CompileError> {
        let m = self.meta(
            checker,
            call,
            &[
                "id",
                "title",
                "version",
                "settingKey",
                "valueType",
                "value",
                "effectiveFrom",
                "effectiveTo",
                "supersedes",
                "review",
                "scopeRequired",
            ],
        )?;
        let id = self.read_setting_id(checker, Self::member(&m, "id", call)?)?;
        let title = self.non_blank(checker, Self::member(&m, "title", call)?, "setting title")?;
        let version = self.non_blank(
            checker,
            Self::member(&m, "version", call)?,
            "setting version",
        )?;
        let setting_key =
            self.non_blank(checker, Self::member(&m, "settingKey", call)?, "settingKey")?;
        let vt_node = Self::member(&m, "valueType", call)?;
        let value_type = self.string(checker, vt_node)?;
        if !matches!(value_type.as_str(), "text" | "number" | "boolean" | "json") {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "valueType must be \"text\", \"number\", \"boolean\", or \"json\"",
                vt_node,
            ));
        }
        let value =
            self.read_setting_value(checker, &value_type, Self::member(&m, "value", call)?)?;
        let (effective_from, effective_to) = self.read_setting_window(checker, &m, call)?;

        let mut payload = Map::new();
        payload.insert("settingVersion".to_owned(), Value::String("0.1".to_owned()));
        payload.insert("settingKey".to_owned(), Value::String(setting_key));
        payload.insert("valueType".to_owned(), Value::String(value_type));
        payload.insert("value".to_owned(), value);
        payload.insert("effectiveFrom".to_owned(), Value::String(effective_from));
        if let Some(et) = effective_to {
            payload.insert("effectiveTo".to_owned(), Value::String(et));
        }
        if let Some(sup_node) = m.get("supersedes") {
            payload.insert(
                "supersedes".to_owned(),
                self.read_setting_supersedes(checker, sup_node)?,
            );
        }
        let review = self.read_setting_review(checker, Self::member(&m, "review", call)?)?;
        payload.insert("review".to_owned(), review);
        let scope_req =
            self.read_setting_scope_required(checker, Self::member(&m, "scopeRequired", call)?)?;
        payload.insert("scopeRequired".to_owned(), Value::Array(scope_req));

        let namespace = id.split('.').next().unwrap_or("").to_owned();
        let artifact = build_envelope(
            EnvelopeMeta {
                kind: "setting".to_owned(),
                artifact_id: id,
                namespace,
                name: title,
                version,
                dependencies: Vec::new(),
            },
            Value::Object(payload),
            options,
        );
        Self::assert_expansion(expansion, &artifact, call)?;
        Ok(LoweredDef {
            artifact,
            labels: None,
            at: None,
        })
    }

    fn read_setting_id(
        &self,
        checker: &mut Checker,
        id_node: P<Node>,
    ) -> Result<String, CompileError> {
        let id = self.non_blank(checker, id_node, "setting id")?;
        let segs: Vec<&str> = id.split('.').collect();
        if id != id.trim()
            || segs.len() < 2
            || segs.iter().any(|s| s.trim().is_empty() || *s != s.trim())
        {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!(
                    "setting id {id:?} must be a dotted identifier with non-empty segments and no leading or trailing whitespace"
                ),
                id_node,
            ));
        }
        Ok(id)
    }

    fn read_setting_window(
        &self,
        checker: &mut Checker,
        m: &Members,
        call: P<Node>,
    ) -> Result<(String, Option<String>), CompileError> {
        let eff_from_node = Self::member(m, "effectiveFrom", call)?;
        let effective_from = self.non_blank(checker, eff_from_node, "effectiveFrom")?;
        let Some(from_ns) = parse_rfc3339_instant(&effective_from) else {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("effectiveFrom {effective_from:?} must be a valid RFC 3339 timestamp"),
                eff_from_node,
            ));
        };
        let mut effective_to: Option<String> = None;
        if let Some(et_node) = m.get("effectiveTo") {
            let et = self.non_blank(checker, et_node, "effectiveTo")?;
            let Some(to_ns) = parse_rfc3339_instant(&et) else {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!("effectiveTo {et:?} must be a valid RFC 3339 timestamp"),
                    et_node,
                ));
            };
            if to_ns <= from_ns {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "effectiveTo must be strictly after effectiveFrom",
                    et_node,
                ));
            }
            effective_to = Some(et);
        }
        Ok((effective_from, effective_to))
    }

    fn read_setting_supersedes(
        &self,
        checker: &mut Checker,
        sup_node: P<Node>,
    ) -> Result<Value, CompileError> {
        let s = self.closed_record(
            checker,
            sup_node,
            &["artifactId", "version"],
            &["artifactId", "version"],
        )?;
        let mut so = Map::new();
        so.insert(
            "artifactId".to_owned(),
            Value::String(self.non_blank(
                checker,
                Self::member(&s, "artifactId", sup_node)?,
                "supersedes.artifactId",
            )?),
        );
        so.insert(
            "version".to_owned(),
            Value::String(self.non_blank(
                checker,
                Self::member(&s, "version", sup_node)?,
                "supersedes.version",
            )?),
        );
        Ok(Value::Object(so))
    }

    fn read_setting_scope_required(
        &self,
        checker: &mut Checker,
        scope_node: P<Node>,
    ) -> Result<Vec<Value>, CompileError> {
        let mut scope_req = Vec::new();
        let mut seen_dims = HashSet::new();
        for item in self.list(checker, scope_node)? {
            let dim = self.non_blank(checker, item, "scopeRequired entry")?;
            if !matches!(
                dim.as_str(),
                "orgId" | "tenantId" | "workspaceId" | "environmentId" | "businessUnitId"
            ) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    format!(
                        "scopeRequired entry {dim:?} must be one of orgId, tenantId, workspaceId, environmentId, businessUnitId"
                    ),
                    item,
                ));
            }
            if !seen_dims.insert(dim.clone()) {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "scopeRequired must not contain duplicate scope dimensions",
                    scope_node,
                ));
            }
            scope_req.push(Value::String(dim));
        }
        if scope_req.is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "scopeRequired must contain at least one scope dimension",
                scope_node,
            ));
        }
        Ok(scope_req)
    }

    fn non_blank(
        &self,
        checker: &mut Checker,
        node: P<Node>,
        label: &str,
    ) -> Result<String, CompileError> {
        let value = self.string(checker, node)?;
        if value.trim().is_empty() {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                format!("{label} must be a non-empty string"),
                node,
            ));
        }
        Ok(value)
    }

    fn read_setting_value(
        &mut self,
        checker: &mut Checker,
        value_type: &str,
        val_node: P<Node>,
    ) -> Result<Value, CompileError> {
        match value_type {
            "text" => Ok(Value::String(self.string(checker, val_node)?)),
            "boolean" => Ok(Value::Bool(self.boolean(checker, val_node)?)),
            "number" => {
                let expr = self.deref(checker, val_node)?;
                let is_js_num = expr.kind() == Kind::NumericLiteral
                    || (expr.kind() == Kind::PrefixUnaryExpression && {
                        let pu = expr.as_prefix_unary_expression();
                        (pu.operator == Kind::MinusToken || pu.operator == Kind::PlusToken)
                            && Self::unwrap(pu.operand).kind() == Kind::NumericLiteral
                    });
                if is_js_num {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "numeric settings require an exact DecimalLiteral string (e.g. \"0.12\"), not a binary JavaScript number (VAL-EXACT-1)",
                        val_node,
                    ));
                }
                if expr.kind() != Kind::StringLiteral
                    && expr.kind() != Kind::NoSubstitutionTemplateLiteral
                {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        "numeric settings require an exact DecimalLiteral string literal",
                        val_node,
                    ));
                }
                let text = expr.text();
                if !is_valid_decimal_literal(text) {
                    return Err(CompileError::new(
                        "NORN-FLOW-LITERAL",
                        format!(
                            "numeric setting value {text:?} is not a valid exact DecimalLiteral"
                        ),
                        val_node,
                    ));
                }
                Ok(Value::String(text.to_owned()))
            }
            _ => self.json(checker, val_node, 0),
        }
    }

    fn read_setting_review(
        &self,
        checker: &mut Checker,
        review_node: P<Node>,
    ) -> Result<Value, CompileError> {
        let r = self.closed_record(
            checker,
            review_node,
            &["state", "reviewedBy", "reviewedAt", "reason"],
            &["state"],
        )?;
        let state_node = Self::member(&r, "state", review_node)?;
        let state = self.string(checker, state_node)?;
        if !matches!(
            state.as_str(),
            "draft" | "proposed" | "approved" | "active" | "rejected" | "superseded"
        ) {
            return Err(CompileError::new(
                "NORN-FLOW-LITERAL",
                "review.state must be \"draft\", \"proposed\", \"approved\", \"active\", \"rejected\", or \"superseded\"",
                state_node,
            ));
        }
        let mut obj = Map::new();
        obj.insert("state".to_owned(), Value::String(state));
        if let Some(by) = r.get("reviewedBy") {
            obj.insert(
                "reviewedBy".to_owned(),
                Value::String(self.non_blank(checker, by, "review.reviewedBy")?),
            );
        }
        if let Some(at) = r.get("reviewedAt") {
            let at_str = self.non_blank(checker, at, "review.reviewedAt")?;
            if parse_rfc3339_instant(&at_str).is_none() {
                return Err(CompileError::new(
                    "NORN-FLOW-LITERAL",
                    "review.reviewedAt must be a valid RFC 3339 timestamp",
                    at,
                ));
            }
            obj.insert("reviewedAt".to_owned(), Value::String(at_str));
        }
        if let Some(reason) = r.get("reason") {
            obj.insert(
                "reason".to_owned(),
                Value::String(self.non_blank(checker, reason, "review.reason")?),
            );
        }
        Ok(Value::Object(obj))
    }
}

/// Validates a `norn_ir::Decimal` wire literal (`VAL-EXACT-1`).
#[must_use]
pub(crate) fn is_valid_decimal_literal(text: &str) -> bool {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    if !is_unsigned_decimal_text(unsigned) {
        return false;
    }
    let (int_part, frac_part) = match unsigned.split_once('.') {
        Some((i, f)) => (i, f),
        None => (unsigned, ""),
    };
    if frac_part.len() > MAX_DECIMAL_SCALE {
        return false;
    }
    let trimmed_frac = frac_part.trim_end_matches('0');
    let digits = format!("{int_part}{trimmed_frac}");
    digits
        .parse::<u128>()
        .is_ok_and(|mantissa| mantissa <= MAX_DECIMAL_MANTISSA)
}

fn is_leap_year(year: i64) -> bool {
    (year.rem_euclid(4) == 0 && year.rem_euclid(100) != 0) || year.rem_euclid(400) == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Howard Hinnant civil-from-days inverse: days since Unix epoch (1970-01-01).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 {
        year.saturating_sub(1)
    } else {
        year
    };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let d = i64::from(day);
    let mp = if m > 2 {
        m.saturating_sub(3)
    } else {
        m.saturating_add(9)
    };
    let doy = mp
        .saturating_mul(153)
        .saturating_add(2)
        .div_euclid(5)
        .saturating_add(d.saturating_sub(1));
    let doe = yoe
        .saturating_mul(365)
        .saturating_add(yoe.div_euclid(4))
        .saturating_sub(yoe.div_euclid(100))
        .saturating_add(doy);
    era.saturating_mul(146_097)
        .saturating_add(doe)
        .saturating_sub(719_468)
}

/// Strict RFC 3339 timestamp parser returning UTC nanoseconds since Unix epoch.
#[must_use]
pub(crate) fn parse_rfc3339_instant(text: &str) -> Option<i128> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    if bytes.get(4) != Some(&b'-') {
        return None;
    }
    let month: u32 = text.get(5..7)?.parse().ok()?;
    if bytes.get(7) != Some(&b'-') {
        return None;
    }
    let day: u32 = text.get(8..10)?.parse().ok()?;
    if !matches!(bytes.get(10), Some(b'T' | b't')) {
        return None;
    }
    let hour: u32 = text.get(11..13)?.parse().ok()?;
    if bytes.get(13) != Some(&b':') {
        return None;
    }
    let minute: u32 = text.get(14..16)?.parse().ok()?;
    if bytes.get(16) != Some(&b':') {
        return None;
    }
    let second: u32 = text.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let rest = text.get(19..)?;
    let (frac_nanos, tz) = if let Some(after_dot) = rest.strip_prefix('.') {
        let frac_end = after_dot
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after_dot.len());
        if frac_end == 0 || frac_end > 9 {
            return None;
        }
        let frac_raw = after_dot.get(..frac_end)?;
        let padded = format!("{frac_raw:0<9}");
        (padded.parse::<i128>().ok()?, after_dot.get(frac_end..)?)
    } else {
        (0_i128, rest)
    };
    let offset_nanos = parse_rfc3339_offset(tz)?;
    let days = i128::from(days_from_civil(year, month, day));
    let secs = days
        .saturating_mul(86_400)
        .saturating_add(i128::from(hour).saturating_mul(3_600))
        .saturating_add(i128::from(minute).saturating_mul(60))
        .saturating_add(i128::from(second));
    Some(
        secs.saturating_mul(1_000_000_000)
            .saturating_sub(offset_nanos)
            .saturating_add(frac_nanos),
    )
}

fn parse_rfc3339_offset(tz: &str) -> Option<i128> {
    if tz == "Z" || tz == "z" {
        return Some(0);
    }
    let sign: i128 = if tz.starts_with('-') {
        -1
    } else if tz.starts_with('+') {
        1
    } else {
        return None;
    };
    if tz.len() != 6 || tz.as_bytes().get(3) != Some(&b':') {
        return None;
    }
    let oh: u32 = tz.get(1..3)?.parse().ok()?;
    let om: u32 = tz.get(4..6)?.parse().ok()?;
    if oh > 23 || om > 59 {
        return None;
    }
    let secs = i128::from(oh)
        .saturating_mul(60)
        .saturating_add(i128::from(om))
        .saturating_mul(60);
    Some(sign.saturating_mul(secs).saturating_mul(1_000_000_000))
}
