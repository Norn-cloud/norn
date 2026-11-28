//! Integration tests for `norn-authoring-py` porting positive and refusal cases
//! from `tools/norn-py/tests/test_norn_py.py` and `tools/norn-py/examples/hr_pilot.py`.

#[path = "suites/schema_parity.rs"]
mod schema_parity;
#[path = "suites/schema_refusals.rs"]
mod schema_refusals;

use std::error::Error;

use norn_authoring_py::{
    CODE_EXPRESSION, CODE_FLOAT_LITERAL, CODE_FORBIDDEN_ATTR, CODE_FORBIDDEN_CALL,
    CODE_FORBIDDEN_CONTROL_FLOW, CODE_FORBIDDEN_IMPORT, CODE_FORBIDDEN_NAME, CODE_FORBIDDEN_SYNTAX,
    CODE_FORMAT, CODE_RAW_EXPR, CODE_SYNTAX, CODE_UNBOUNDED, CODE_UNKNOWN_FIELD, FormatConfig,
    FormatQuoteStyle, LowerContext, PyAuthoringError, SandboxLimits, ZenLiteral,
    format_exact_decimal, format_python_source, format_python_source_with, is_formatted,
    lower_callback_source, lower_callback_source_with_limits, lower_module_callbacks,
    preflight_source, preflight_source_with_limits, render_zen_string, validate_callback_source,
    validate_callback_source_with_limits,
};

type TestResult = Result<(), Box<dyn Error>>;

fn expect_err<T>(
    result: Result<T, PyAuthoringError>,
    context: &str,
) -> Result<PyAuthoringError, Box<dyn Error>> {
    match result {
        Err(err) => Ok(err),
        Ok(_) => Err(format!("expected refusal for `{context}`, got Ok").into()),
    }
}

#[test]
fn sandbox_blocks_forbidden_imports_builtins_dunders_and_syntax() -> TestResult {
    let cases: &[(&str, &str)] = &[
        ("import os\n", CODE_FORBIDDEN_IMPORT),
        ("from typing import sys\n", CODE_FORBIDDEN_IMPORT),
        ("from enum import sys\n", CODE_FORBIDDEN_IMPORT),
        (
            "from norn_py import elaborate_file\n",
            CODE_FORBIDDEN_IMPORT,
        ),
        ("from norn_py import *\n", CODE_FORBIDDEN_IMPORT),
        ("from . import foo\n", CODE_FORBIDDEN_IMPORT),
        ("from dataclasses import dataclass\n", CODE_FORBIDDEN_IMPORT),
        (
            "x = ().__class__.__bases__[0].__subclasses__()\n",
            CODE_FORBIDDEN_ATTR,
        ),
        ("x = \"{0.__class__}\".format(1)\n", CODE_FORBIDDEN_ATTR),
        (
            "x = \"{a.__globals__}\".format_map({\"a\": 1})\n",
            CODE_FORBIDDEN_ATTR,
        ),
        ("x = eval('1+1')\n", CODE_FORBIDDEN_CALL),
        ("f = eval\n", CODE_FORBIDDEN_CALL),
        ("x = getattr(str, 'upper')\n", CODE_FORBIDDEN_CALL),
        ("x = type('Evil', (), {})\n", CODE_FORBIDDEN_CALL),
        ("x = globals()\n", CODE_FORBIDDEN_CALL),
        ("x = dir()\n", CODE_FORBIDDEN_CALL),
        ("open('/etc/passwd')\n", CODE_FORBIDDEN_CALL),
        ("x = 1.25\n", CODE_FLOAT_LITERAL),
        ("x = 1 + 2j\n", CODE_FLOAT_LITERAL),
        ("import typing as __evil\n", CODE_FORBIDDEN_NAME),
        ("from decimal import Decimal as eval\n", CODE_FORBIDDEN_NAME),
        ("def f(__x):\n    return __x\n", CODE_FORBIDDEN_NAME),
        ("def f(eval):\n    return eval\n", CODE_FORBIDDEN_NAME),
        ("class __Secret:\n    pass\n", CODE_FORBIDDEN_SYNTAX),
        ("def eval(x):\n    return x\n", CODE_FORBIDDEN_SYNTAX),
        (
            "try:\n    x = 1\nexcept Exception:\n    pass\n",
            CODE_FORBIDDEN_SYNTAX,
        ),
        ("async def f(s):\n    return s\n", CODE_FORBIDDEN_SYNTAX),
        (
            "class Evil(metaclass=int):\n    pass\n",
            CODE_FORBIDDEN_SYNTAX,
        ),
        (
            "class Bad:\n    def __init__(self):\n        pass\n",
            CODE_FORBIDDEN_SYNTAX,
        ),
        ("for x in (1, 2):\n    pass\n", CODE_FORBIDDEN_SYNTAX),
        ("while False:\n    pass\n", CODE_FORBIDDEN_SYNTAX),
        ("if True:\n    x = 1\n", CODE_FORBIDDEN_CONTROL_FLOW),
        ("x = 1 if True else 2\n", CODE_FORBIDDEN_CONTROL_FLOW),
        ("x = [y for y in (1, 2)]\n", CODE_FORBIDDEN_CONTROL_FLOW),
    ];

    for &(snippet, expected_code) in cases {
        let err = expect_err(preflight_source(snippet, "bad.py"), snippet)?;
        assert_eq!(
            err.code(),
            expected_code,
            "snippet `{snippet}` expected {expected_code}, got {err}"
        );
    }
    Ok(())
}

#[test]
fn sandbox_enforces_source_byte_node_and_depth_budgets() -> TestResult {
    let tiny_byte_limit = SandboxLimits {
        max_source_bytes: 16,
        ..SandboxLimits::default()
    };
    let err_bytes = expect_err(
        preflight_source_with_limits("x = 12345678901234567890\n", "big.py", tiny_byte_limit),
        "byte limit",
    )?;
    assert_eq!(err_bytes.code(), CODE_UNBOUNDED);

    let tiny_node_limit = SandboxLimits {
        max_ast_nodes: 3,
        ..SandboxLimits::default()
    };
    let err_nodes = expect_err(
        preflight_source_with_limits("a = 1\nb = 2\nc = 3\nd = 4\n", "many.py", tiny_node_limit),
        "node limit",
    )?;
    assert_eq!(err_nodes.code(), CODE_UNBOUNDED);

    // Sub-nodes like import aliases are counted in AST node totals (matching CPython ast.walk).
    let alias_report = preflight_source(
        "from typing import Annotated, ClassVar, Final, Literal, Optional, Protocol, Union\n",
        "aliases.py",
    )?;
    assert!(
        alias_report.ast_node_count >= 8,
        "expected import + 7 aliases to count >= 8 nodes, got {}",
        alias_report.ast_node_count
    );

    let tiny_depth_limit = SandboxLimits {
        max_ast_depth: 4,
        ..SandboxLimits::default()
    };
    let err_depth = expect_err(
        preflight_source_with_limits("x = [[[[[1]]]]]\n", "deep.py", tiny_depth_limit),
        "depth limit",
    )?;
    assert_eq!(err_depth.code(), CODE_UNBOUNDED);

    let err_syntax = expect_err(preflight_source("def broken(:\n", "broken.py"), "syntax")?;
    assert_eq!(err_syntax.code(), CODE_SYNTAX);
    Ok(())
}

#[test]
fn standalone_callbacks_enforce_ast_node_and_depth_budgets_before_recursion() -> TestResult {
    let tight_depth = SandboxLimits {
        max_ast_depth: 8,
        ..SandboxLimits::default()
    };
    let nested = "lambda s: not_(not_(not_(not_(not_(s.hours > 0)))))";
    let err_val_depth = expect_err(
        validate_callback_source_with_limits(nested, "deep_cb.py", tight_depth),
        "validate depth",
    )?;
    assert_eq!(err_val_depth.code(), CODE_UNBOUNDED);

    let err_low_depth = expect_err(
        lower_callback_source_with_limits(
            nested,
            "deep_cb.py",
            &LowerContext::default(),
            tight_depth,
        ),
        "lower depth",
    )?;
    assert_eq!(err_low_depth.code(), CODE_UNBOUNDED);

    let tight_nodes = SandboxLimits {
        max_ast_nodes: 5,
        ..SandboxLimits::default()
    };
    let err_val_nodes = expect_err(
        validate_callback_source_with_limits("lambda s: s.hours > 0", "many_cb.py", tight_nodes),
        "validate nodes",
    )?;
    assert_eq!(err_val_nodes.code(), CODE_UNBOUNDED);

    let err_low_nodes = expect_err(
        lower_callback_source_with_limits(
            "lambda s: s.hours > 0",
            "many_cb.py",
            &LowerContext::default(),
            tight_nodes,
        ),
        "lower nodes",
    )?;
    assert_eq!(err_low_nodes.code(), CODE_UNBOUNDED);

    // Validate and lower must enforce identical AST node and depth boundaries for standalone lambdas.
    let boundary_src = "lambda s: s.hours > 0";
    for max_nodes in 1..=20 {
        let limits = SandboxLimits {
            max_ast_nodes: max_nodes,
            ..SandboxLimits::default()
        };
        let val = validate_callback_source_with_limits(boundary_src, "boundary.py", limits);
        let low = lower_callback_source_with_limits(
            boundary_src,
            "boundary.py",
            &LowerContext::default(),
            limits,
        );
        assert_eq!(
            val.is_ok(),
            low.is_ok(),
            "validate vs lower diverged at max_ast_nodes={max_nodes}"
        );
        if let (Err(ve), Err(le)) = (val, low) {
            assert_eq!(ve.code(), CODE_UNBOUNDED);
            assert_eq!(le.code(), CODE_UNBOUNDED);
        }
    }
    for max_depth in 1..=12 {
        let limits = SandboxLimits {
            max_ast_depth: max_depth,
            ..SandboxLimits::default()
        };
        let val = validate_callback_source_with_limits(boundary_src, "boundary.py", limits);
        let low = lower_callback_source_with_limits(
            boundary_src,
            "boundary.py",
            &LowerContext::default(),
            limits,
        );
        assert_eq!(
            val.is_ok(),
            low.is_ok(),
            "validate vs lower diverged at max_ast_depth={max_depth}"
        );
        if let (Err(ve), Err(le)) = (val, low) {
            assert_eq!(ve.code(), CODE_UNBOUNDED);
            assert_eq!(le.code(), CODE_UNBOUNDED);
        }
    }

    // Default depth budget (128) must also reject deeply nested standalone callbacks cleanly.
    let mut deeply_nested = String::from("lambda s: ");
    for _ in 0..70 {
        deeply_nested.push_str("not_(");
    }
    deeply_nested.push_str("s.hours > 0");
    for _ in 0..70 {
        deeply_nested.push(')');
    }
    let err_default_val = expect_err(
        validate_callback_source(&deeply_nested, "deep_default.py"),
        "default validate depth",
    )?;
    assert_eq!(err_default_val.code(), CODE_UNBOUNDED);

    let err_default_low = expect_err(
        lower_callback_source(&deeply_nested, "deep_default.py", &LowerContext::default()),
        "default lower depth",
    )?;
    assert_eq!(err_default_low.code(), CODE_UNBOUNDED);
    Ok(())
}

#[test]
fn sandbox_accepts_hr_pilot_and_lowers_all_callbacks() -> TestResult {
    let hr_pilot = include_str!("../../../tools/norn-py/examples/hr_pilot.py");
    let report = preflight_source(hr_pilot, "tools/norn-py/examples/hr_pilot.py")?;

    assert!(report.spans.contains_key("HR_MARKINGS"));
    assert!(report.spans.contains_key("TimeSpan"));
    assert!(report.spans.contains_key("Employee"));
    assert!(report.spans.contains_key("LeaveRequest"));
    assert!(report.spans.contains_key("Shift"));
    assert!(report.spans.contains_key("shift_bounds"));
    assert!(report.ast_node_count > 50);

    let callbacks = lower_module_callbacks(
        hr_pilot,
        "tools/norn-py/examples/hr_pilot.py",
        &LowerContext::default(),
    )?;

    let proper_interval = callbacks
        .get("TimeSpan._proper_interval")
        .ok_or("missing TimeSpan._proper_interval")?;
    assert_eq!(
        proper_interval.to_zen()?,
        "timestamp(self.starts_at) < timestamp(self.ends_at)"
    );

    let positive_leave = callbacks
        .get("_positive_leave_days")
        .ok_or("missing _positive_leave_days")?;
    assert_eq!(positive_leave.to_zen()?, "self.days > 0.00");

    let shift_bounds = callbacks
        .get("shift_bounds")
        .ok_or("missing shift_bounds")?;
    assert_eq!(
        shift_bounds.to_zen()?,
        "((timestamp(self.starts_at) < timestamp(self.ends_at)) and (self.hours > 0.00)) and (self.hours <= 12.00)"
    );
    Ok(())
}

#[test]
fn callback_validation_rejects_non_declarative_constructs() -> TestResult {
    let bad_callbacks: &[(&str, &str)] = &[
        (
            "lambda s: s.ends_at if s.ends_at is None else s.starts_at",
            CODE_FORBIDDEN_CONTROL_FLOW,
        ),
        ("lambda s: s.a and s.b", CODE_FORBIDDEN_CONTROL_FLOW),
        ("lambda s: s.a or s.b", CODE_FORBIDDEN_CONTROL_FLOW),
        ("lambda s: not s.a", CODE_FORBIDDEN_CONTROL_FLOW),
        ("lambda s: s.a is None", CODE_FORBIDDEN_CONTROL_FLOW),
        ("lambda s: 'x' in s.a", CODE_FORBIDDEN_CONTROL_FLOW),
        ("lambda s: 0 < s.a <= 12", CODE_FORBIDDEN_CONTROL_FLOW),
        ("lambda s: [x for x in (1, 2)]", CODE_FORBIDDEN_CONTROL_FLOW),
        ("lambda s: (lambda x: x)(s.a)", CODE_FORBIDDEN_CALL),
        ("lambda s: len(s.a) > 0", CODE_FORBIDDEN_CALL),
        ("lambda s: s.a.upper() == 'X'", CODE_FORBIDDEN_CALL),
        ("lambda s: s.hours > 8.5", CODE_FLOAT_LITERAL),
        ("lambda s: s.__class__", CODE_FORBIDDEN_ATTR),
        (
            "def bad_if(s):\n    if s.a:\n        return s.a.eq('x')\n    return s.b.eq('y')\n",
            CODE_FORBIDDEN_CONTROL_FLOW,
        ),
        (
            "def two_params(a, b):\n    return a.x > b.x\n",
            CODE_FORBIDDEN_CONTROL_FLOW,
        ),
    ];

    for &(src, expected_code) in bad_callbacks {
        let err = expect_err(validate_callback_source(src, "cb.py"), src)?;
        assert_eq!(
            err.code(),
            expected_code,
            "callback `{src}` expected {expected_code}, got {err}"
        );
    }
    Ok(())
}

fn parity_lower_context() -> LowerContext {
    LowerContext::default()
        .with_allowed_fields([
            "hours",
            "starts_at",
            "ends_at",
            "start_date",
            "end_date",
            "status",
            "name",
        ])
        .with_constant("LeaveStatus.DRAFT", ZenLiteral::Str("draft".to_owned()))
        .with_constant("Prio.HIGH", ZenLiteral::Int("3".to_owned()))
}

#[test]
fn expr_lowering_matches_norn_py_zen_parity() -> TestResult {
    let ctx = parity_lower_context();

    let parity_cases: &[(&str, &str)] = &[
        ("lambda s: s.hours > Decimal('0')", "self.hours > 0"),
        ("lambda s: lit(True)", "true"),
        (
            "lambda s: all_of(s.hours > 0, lit(True))",
            "(self.hours > 0) and true",
        ),
        (
            "lambda s: timestamp_before(s.starts_at, s.ends_at)",
            "timestamp(self.starts_at) < timestamp(self.ends_at)",
        ),
        (
            "lambda s: timestamp_at_or_before(s.starts_at, s.ends_at)",
            "timestamp(self.starts_at) <= timestamp(self.ends_at)",
        ),
        (
            "lambda s: date_before(s.start_date, s.end_date)",
            "date(self.start_date) < date(self.end_date)",
        ),
        (
            "lambda s: date_before(parse_date(s.start_date), s.end_date)",
            "date(date(self.start_date)) < date(self.end_date)",
        ),
        (
            "lambda s: between(s.hours, Decimal('1.00'), Decimal('12.00'))",
            "(self.hours >= 1.00) and (self.hours <= 12.00)",
        ),
        (
            "lambda s: between(s.hours, Decimal('1.00'), Decimal('12.00'), inclusive=False)",
            "(self.hours > 1.00) and (self.hours < 12.00)",
        ),
        (
            "lambda s: all_of(intervals_overlap(s.starts_at, s.ends_at, s.starts_at, s.ends_at), interval_gap_seconds(s.starts_at, s.ends_at, s.starts_at, s.ends_at) >= Decimal('39600'))",
            "intervals_overlap(timestamp(self.starts_at), timestamp(self.ends_at), timestamp(self.starts_at), timestamp(self.ends_at)) and (interval_gap_seconds(timestamp(self.starts_at), timestamp(self.ends_at), timestamp(self.starts_at), timestamp(self.ends_at)) >= 39600)",
        ),
        ("lambda s: -s.hours", "-self.hours"),
        ("lambda s: -(-s.hours)", "-(-self.hours)"),
        ("lambda s: +s.hours", "self.hours"),
        ("lambda s: Decimal('24') - s.hours", "24 - self.hours"),
        ("lambda s: 2 * s.hours", "2 * self.hours"),
        (
            "lambda s: s.status == LeaveStatus.DRAFT",
            "self.status == 'draft'",
        ),
        ("lambda s: s.hours == Prio.HIGH", "self.hours == 3"),
        ("lambda s: s.name.is_not_null()", "self.name != null"),
        ("lambda s: s.name.is_null()", "self.name == null"),
        ("lambda s: norn_len(s.name) > 0", "len(self.name) > 0"),
        (
            "lambda s: contains(s.name, 'hr')",
            "contains(self.name, 'hr')",
        ),
        (
            "lambda s: when(s.hours > 8, Decimal('1.5'), Decimal('1.0'))",
            "(self.hours > 8) ? 1.5 : 1.0",
        ),
        (
            "lambda s: not_(s.status.eq('draft'))",
            "not (self.status == 'draft')",
        ),
        (
            "lambda s: days_between(s.start_date, add_days(s.start_date, 5)) == 5",
            "days_between(date(self.start_date), add_days(date(self.start_date), 5)) == 5",
        ),
        (
            "lambda s: seconds_between(s.starts_at, add_seconds(s.starts_at, 60)) == 60",
            "seconds_between(timestamp(self.starts_at), add_seconds(timestamp(self.starts_at), 60)) == 60",
        ),
    ];

    for &(src, expected_zen) in parity_cases {
        let expr = lower_callback_source(src, "parity.py", &ctx)?;
        assert_eq!(
            expr.to_zen()?,
            expected_zen,
            "ZEN parity mismatch for `{src}`"
        );
    }
    Ok(())
}

#[test]
fn expr_lowering_refuses_raw_literals_and_unknown_fields() -> TestResult {
    let ctx = parity_lower_context();

    // Raw literals are refused as callback return values and as logical combinator arguments.
    let raw_literal_cases = [
        "lambda s: True",
        "lambda s: 42",
        "lambda s: 'raw'",
        "lambda s: Decimal('1.00')",
        "lambda s: LeaveStatus.DRAFT",
        "lambda s: 1 < 2",
        "lambda s: 1 + 2",
        "def raw_ret(s):\n    return True\n",
        "lambda s: all_of(s.hours > 0, True)",
        "lambda s: any_of(False, s.hours > 0)",
        "lambda s: not_(True)",
        "lambda s: when(True, s.hours, 0)",
        "lambda s: (s.hours > 0) & True",
    ];
    for src in raw_literal_cases {
        let err = expect_err(lower_callback_source(src, "raw.py", &ctx), src)?;
        assert_eq!(
            err.code(),
            CODE_RAW_EXPR,
            "expected {CODE_RAW_EXPR} for `{src}`, got {err}"
        );
    }

    let err_unknown = expect_err(
        lower_callback_source("lambda s: s.nonexistent > 0", "unk.py", &ctx),
        "unknown field",
    )?;
    assert_eq!(err_unknown.code(), CODE_UNKNOWN_FIELD);
    Ok(())
}

#[test]
fn exact_decimal_and_zen_string_bounds_enforced() -> TestResult {
    assert_eq!(format_exact_decimal("0", None)?, "0");
    assert_eq!(format_exact_decimal("-0", None)?, "-0");
    assert_eq!(format_exact_decimal("0.00", None)?, "0.00");
    assert_eq!(format_exact_decimal("-0.00", None)?, "-0.00");
    assert_eq!(format_exact_decimal("0e3", None)?, "0");
    assert_eq!(format_exact_decimal("-0e3", None)?, "-0");
    assert_eq!(format_exact_decimal("0.00e3", None)?, "0");
    assert_eq!(format_exact_decimal("0e-3", None)?, "0.000");
    assert_eq!(format_exact_decimal("-0e-3", None)?, "-0.000");
    assert_eq!(format_exact_decimal("1e3", None)?, "1000");
    assert_eq!(format_exact_decimal("1.50e-2", None)?, "0.0150");
    assert_eq!(format_exact_decimal("1_000.50", None)?, "1000.50");
    assert_eq!(
        format_exact_decimal("1_000.5_0e+1_0", None)?,
        "10005000000000"
    );
    assert_eq!(
        format_exact_decimal("\u{0661}\u{0662}.\u{0663}", None)?,
        "12.3"
    );
    assert_eq!(format_exact_decimal("\u{0660}e3", None)?, "0");
    assert_eq!(format_exact_decimal("\u{ff11}\u{ff12}", None)?, "12");

    for bad in [
        "",
        "_",
        ".",
        "1e1000000000",
        "0e-1000000000",
        "0e1001",
        "NaN",
        "N_aN",
        "Infinity",
        "-Infinity",
        "not-a-num",
    ] {
        let err = expect_err(format_exact_decimal(bad, None), bad)?;
        assert_eq!(err.code(), CODE_EXPRESSION, "expected refusal for `{bad}`");
    }

    assert_eq!(render_zen_string("hello", None)?, "'hello'");
    assert_eq!(render_zen_string("it's", None)?, "\"it's\"");
    assert_eq!(
        expect_err(render_zen_string("bad\x7fdel", None), "del")?.code(),
        CODE_EXPRESSION
    );
    assert_eq!(
        expect_err(render_zen_string("both ' and \"", None), "both quotes")?.code(),
        CODE_EXPRESSION
    );
    Ok(())
}

#[test]
fn formatter_is_deterministic_and_idempotent() -> TestResult {
    let messy = "from decimal import Decimal\n\ndef   shift_ok( s ):\n  return  s.hours>Decimal( '0.00' )\n";
    let once = format_python_source(messy)?;
    let twice = format_python_source(&once)?;
    assert_eq!(once, twice);
    assert!(is_formatted(&once)?);
    assert!(!is_formatted(messy)?);
    assert_eq!(
        once,
        "from decimal import Decimal\n\n\ndef shift_ok(s):\n    return s.hours > Decimal(\"0.00\")\n"
    );

    let single_quote_cfg = FormatConfig {
        line_width: 88,
        indent_width: 4,
        quote_style: FormatQuoteStyle::Single,
    };
    let single = format_python_source_with(messy, single_quote_cfg, "messy.py")?;
    assert!(single.contains("Decimal('0.00')"));

    let bad_cfg = FormatConfig {
        line_width: 0,
        indent_width: 4,
        quote_style: FormatQuoteStyle::Double,
    };
    let err_cfg = expect_err(
        format_python_source_with("x = 1\n", bad_cfg, "x.py"),
        "zero line width",
    )?;
    assert_eq!(err_cfg.code(), CODE_FORMAT);

    let err_syntax = expect_err(format_python_source("def broken(:\n"), "bad syntax")?;
    assert_eq!(err_syntax.code(), CODE_SYNTAX);
    Ok(())
}
