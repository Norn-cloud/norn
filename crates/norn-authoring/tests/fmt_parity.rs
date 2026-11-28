//! Formatter parity integration tests for `norn-authoring` (`FMT-SEMANTIC-1`).

use norn_authoring::{AuthoringError, format_source};

#[test]
fn removes_semicolons_and_normalizes_spacing() -> Result<(), AuthoringError> {
    let input = "export const x={a:1,b:2};\nexport const sum=1+2;\n";
    let formatted = format_source("/product.ts", input)?;
    assert!(
        !formatted.contains(';'),
        "expected semicolons removed: {formatted}"
    );
    assert!(
        formatted.contains("export const x = { a: 1, b: 2 }"),
        "expected brace and operator spacing: {formatted}"
    );
    assert!(
        formatted.contains("export const sum = 1 + 2"),
        "expected binary operator spacing: {formatted}"
    );
    assert!(formatted.ends_with('\n') && !formatted.ends_with("\n\n"));
    Ok(())
}

#[test]
fn uses_two_space_indentation() -> Result<(), AuthoringError> {
    let input = "export function add(a: number, b: number) {\n\tif (a > 0) {\n\t\treturn a + b;\n\t}\n\treturn b;\n}\n";
    let formatted = format_source("/indent.ts", input)?;
    assert!(
        !formatted.contains('\t'),
        "expected tabs converted to spaces: {formatted}"
    );
    assert!(
        formatted.contains("\n  if (a > 0) {\n    return a + b\n  }\n"),
        "expected 2-space indent: {formatted}"
    );
    Ok(())
}

#[test]
fn fmt_semantic_1_preserves_unused_imports_while_sorting() -> Result<(), Box<dyn std::error::Error>>
{
    let input = "import { z, a } from \"./local.ts\";\nimport { Norn } from \"@norn/authoring\";\nimport data from \"./definition.json\";\nexport const x = 1;\n";
    let formatted = format_source("/unused.ts", input)?;
    assert!(
        formatted.contains("\"@norn/authoring\""),
        "unused @norn/authoring import must be preserved: {formatted}"
    );
    assert!(
        formatted.contains("\"./definition.json\""),
        "unused definition import must be preserved: {formatted}"
    );
    assert!(
        formatted.contains("import { a, z } from \"./local.ts\""),
        "named specifiers must be sorted non-destructively: {formatted}"
    );
    let norn_pos = formatted
        .find("\"@norn/authoring\"")
        .ok_or("@norn/authoring missing")?;
    let def_pos = formatted
        .find("\"./definition.json\"")
        .ok_or("./definition.json missing")?;
    let local_pos = formatted
        .find("\"./local.ts\"")
        .ok_or("./local.ts missing")?;
    assert!(
        norn_pos < def_pos && def_pos < local_pos,
        "imports should be sorted deterministically: {formatted}"
    );
    Ok(())
}

#[test]
fn normalizes_crlf_and_trailing_newlines() -> Result<(), AuthoringError> {
    let input = "export const a = 1;\r\nexport const b = 2;\r\n\r\n\r\n";
    let formatted = format_source("/crlf.ts", input)?;
    assert!(
        !formatted.contains('\r'),
        "CRLF must be normalized to LF: {formatted:?}"
    );
    assert_eq!(formatted, "export const a = 1\nexport const b = 2\n");
    Ok(())
}

#[test]
fn removes_final_semicolon_before_whitespace_only_trailing_lines() -> Result<(), AuthoringError> {
    let input = "export const a = 1;\r\n   \r\n\t\r\n";
    let formatted = format_source("/trailing_ws.ts", input)?;
    assert_eq!(formatted, "export const a = 1\n");

    let imports_only = "import { b, a } from \"./helpers.ts\";\r\n\r\n";
    let formatted_imports = format_source("/imports_only.ts", imports_only)?;
    assert_eq!(formatted_imports, "import { a, b } from \"./helpers.ts\"\n");
    Ok(())
}

#[test]
fn format_is_idempotent() -> Result<(), AuthoringError> {
    let input = "import { b, a } from \"./helpers.ts\";\r\nimport { Norn } from \"@norn/authoring\";\r\nexport const obj={k:1+2,m:[1,2]};\r\n\r\n";
    let once = format_source("/idempotent.ts", input)?;
    assert_eq!(
        once,
        "import { Norn } from \"@norn/authoring\"\nimport { a, b } from \"./helpers.ts\"\nexport const obj = { k: 1 + 2, m: [1, 2] }\n"
    );
    let twice = format_source("/idempotent.ts", &once)?;
    assert_eq!(once, twice, "formatting must be idempotent");
    Ok(())
}
