use norn_authoring::format_source;

#[test]
fn removes_semicolons_and_normalizes_spacing() {
    let input = "export const x={a:1,b:2};\nexport const sum=1+2;\n";
    let formatted = format_source("/product.ts", input).expect("format should succeed");
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
}

#[test]
fn uses_two_space_indentation() {
    let input = "export function add(a: number, b: number) {\n\tif (a > 0) {\n\t\treturn a + b;\n\t}\n\treturn b;\n}\n";
    let formatted = format_source("/indent.ts", input).expect("format should succeed");
    assert!(
        !formatted.contains('\t'),
        "expected tabs converted to spaces: {formatted}"
    );
    assert!(
        formatted.contains("\n  if (a > 0) {\n    return a + b\n  }\n"),
        "expected 2-space indent: {formatted}"
    );
}

#[test]
fn fmt_semantic_1_preserves_unused_imports_while_sorting() {
    let input = "import { z, a } from \"./local.ts\";\nimport { Norn } from \"@norn/authoring\";\nimport data from \"./definition.json\";\nexport const x = 1;\n";
    let formatted = format_source("/unused.ts", input).expect("format should succeed");
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
        .expect("@norn/authoring present");
    let def_pos = formatted
        .find("\"./definition.json\"")
        .expect("./definition.json present");
    let local_pos = formatted
        .find("\"./local.ts\"")
        .expect("./local.ts present");
    assert!(
        norn_pos < def_pos && def_pos < local_pos,
        "imports should be sorted deterministically: {formatted}"
    );
}

#[test]
fn normalizes_crlf_and_trailing_newlines() {
    let input = "export const a = 1;\r\nexport const b = 2;\r\n\r\n\r\n";
    let formatted = format_source("/crlf.ts", input).expect("format should succeed");
    assert!(
        !formatted.contains('\r'),
        "CRLF must be normalized to LF: {formatted:?}"
    );
    assert_eq!(formatted, "export const a = 1\nexport const b = 2\n");
}

#[test]
fn format_is_idempotent() {
    let input = "import { b, a } from \"./helpers.ts\";\r\nimport { Norn } from \"@norn/authoring\";\r\nexport const obj={k:1+2,m:[1,2]};\r\n\r\n";
    let once = format_source("/idempotent.ts", input).expect("first format pass");
    let twice = format_source("/idempotent.ts", &once).expect("second format pass");
    assert_eq!(once, twice, "formatting must be idempotent");
}
