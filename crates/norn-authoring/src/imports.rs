//! Static `TypeScript` relative module specifier extraction backed by `tsrs_parser`.

use tsrs_ast::{ExternalModuleIndicatorOptions, Kind, Node, SourceFile, SourceFileParseOptions};
use tsrs_core::arena::Region;
use tsrs_core::{P, ensure_script_kind_from_file_name, tspath};
use tsrs_parser::parse_source_file_owned;

use crate::checker::resolve::{node_end, node_start, node_text_static};

fn is_properly_terminated_string_literal(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let (Some(&first), Some(&last)) = (bytes.first(), bytes.last()) else {
        return false;
    };
    if bytes.len() < 2 || (first != b'"' && first != b'\'') || last != first {
        return false;
    }
    let Some(inner) = bytes.get(1..bytes.len().saturating_sub(1)) else {
        return false;
    };
    let trailing_backslashes = inner
        .iter()
        .rev()
        .take_while(|&&byte| byte == b'\\')
        .count();
    trailing_backslashes.is_multiple_of(2)
}

fn has_parse_error_in_span(sf: P<SourceFile>, start: u32, end: u32) -> bool {
    sf.diagnostics().iter().any(|&diag| {
        let Some(diag_start) = diag.pos() else {
            return false;
        };
        let diag_end = diag.end().unwrap_or(diag_start);
        if diag_end > diag_start {
            diag_start < end && diag_end > start
        } else {
            diag_start >= start && diag_start < end
        }
    })
}

fn statement_module_specifier(stmt: P<Node>) -> Option<P<Node>> {
    if stmt.kind() == Kind::ImportDeclaration {
        Some(stmt.as_import_declaration().module_specifier)
    } else if stmt.kind() == Kind::ExportDeclaration {
        stmt.as_export_declaration().module_specifier
    } else {
        None
    }
}

/// Extracts static relative (`./` and `../`) module specifiers from top-level
/// `TypeScript` `import` and `export ... from` declarations using `tsrs_parser`.
///
/// Parses `source` with the `ScriptKind` inferred from `file_name` (such as
/// `.ts` versus `.tsx`), walks top-level `ImportDeclaration` and
/// `ExportDeclaration` statements, skips missing, unterminated, or malformed
/// string literals, and returns the AST-decoded `UTF-8` specifiers (with
/// `ECMA-262` escapes such as `\x..`, `\uXXXX`, and `\u{...}` decoded) in
/// source order.
#[must_use]
pub fn extract_relative_module_specifiers(source: &str, file_name: &str) -> Vec<String> {
    let effective_name = if file_name.is_empty() {
        "input.ts"
    } else {
        file_name
    };
    let region = Region::new_scratch(64_usize.saturating_mul(1_024));
    {
        let _scope = region.enter();
        let path = tspath::to_path(effective_name, "/", true);
        let opts = SourceFileParseOptions {
            file_name: effective_name.to_owned(),
            path,
            external_module_indicator_options: ExternalModuleIndicatorOptions::default(),
        };
        let script_kind = ensure_script_kind_from_file_name(effective_name);
        let source_file = parse_source_file_owned(opts, source.to_owned(), script_kind);

        let mut specifiers = Vec::new();
        for &stmt in source_file.statements.nodes() {
            let Some(specifier) = statement_module_specifier(stmt) else {
                continue;
            };
            if specifier.kind() != Kind::StringLiteral {
                continue;
            }
            let raw = node_text_static(source_file, specifier);
            if !is_properly_terminated_string_literal(raw) {
                continue;
            }
            let start = node_start(source_file, specifier);
            let end = node_end(specifier);
            if has_parse_error_in_span(source_file, start, end) {
                continue;
            }
            let decoded = specifier.text();
            if decoded.starts_with("./") || decoded.starts_with("../") {
                specifiers.push(decoded.to_owned());
            }
        }
        specifiers
    }
}

#[cfg(test)]
mod tests {
    use super::extract_relative_module_specifiers;

    #[test]
    fn handles_regex_literal_with_unbalanced_delimiters_before_import() {
        let source = "const pattern = /[(]/;\nimport { helper } from \"./support/helper\";\n";
        assert_eq!(
            extract_relative_module_specifiers(source, "flow.ts"),
            vec!["./support/helper"]
        );
    }

    #[test]
    fn extracts_multiline_imports_and_reexports() {
        let source = r#"
import { Effect } from "effect"
import {
  alpha,
  beta as renamedBeta,
}
  from "./support/helper"
import type {
  ItemId,
  from as fromAlias,
}
  from "./types/item"
import defaultVal
  from './default-mod'
import * as ns
  from "../shared/ns"
import "./side-effect"
import def, {
  extra,
} from "./mixed"
import from from "./from-default"
export {
  alpha,
}
  from "./reexport-named"
export *
  from "./reexport-all"
export * as reNs
  from "./reexport-ns"
export type {
  ItemId,
}
  from "./reexport-types"
export type * from "./reexport-type-all"
"#;
        assert_eq!(
            extract_relative_module_specifiers(source, "module.ts"),
            vec![
                "./support/helper",
                "./types/item",
                "./default-mod",
                "../shared/ns",
                "./side-effect",
                "./mixed",
                "./from-default",
                "./reexport-named",
                "./reexport-all",
                "./reexport-ns",
                "./reexport-types",
                "./reexport-type-all",
            ]
        );
    }

    #[test]
    fn decodes_hex_and_unicode_escapes_in_module_specifiers() {
        let source = r#"
import { helper } from "./support/\x68elper"
import { other } from "./support/\u0068elper"
export { helper } from "./support/\u{68}elper"
"#;
        assert_eq!(
            extract_relative_module_specifiers(source, "escapes.ts"),
            vec!["./support/helper", "./support/helper", "./support/helper"]
        );
    }

    #[test]
    fn skips_unterminated_and_malformed_specifier_literals() {
        assert_eq!(
            extract_relative_module_specifiers("import { x } from \"./broken", "broken.ts"),
            Vec::<String>::new()
        );
        assert_eq!(
            extract_relative_module_specifiers("import { x } from \"./broken\\\"", "broken.ts"),
            Vec::<String>::new()
        );
        let multiline_broken = "import { x } from \"./broken\nimport { ok } from \"./good\"\n";
        assert_eq!(
            extract_relative_module_specifiers(multiline_broken, "broken.ts"),
            vec!["./good"]
        );
        let bad_escape = "import { x } from \"./bad/\\xZZ\"\nimport { ok } from \"./good\"\n";
        assert_eq!(
            extract_relative_module_specifiers(bad_escape, "bad_escape.ts"),
            vec!["./good"]
        );
    }

    #[test]
    fn ignores_comments_strings_templates_and_non_imports() {
        let source = r#"
// import { fakeLine } from "./fake-line"
/*
  import { fakeBlock }
    from "./fake-block"
  export * from "./fake-export"
*/
import {
  // from "./fake-in-clause-line"
  real, /* from "./fake-in-clause-block" */
} /* comment */ from "./real-module"

const single = 'import { a } from "./fake-single" \'still string\' import b from "./fake-single-2"'
const doubled = "export * from \"./fake-double\""
const tpl = `
  import { tplFake } from "./fake-template"
  ${`nested \` ${obj.import} import x from "./fake-nested"`}
  export { y } from "./fake-template-tail"
`
const meta = import.meta
const dyn = import("./dynamic-ignored")
const member = holder.import
const record = { import: 1, export: 2 }
export const localVal = 1
export { localVal }
"#;
        assert_eq!(
            extract_relative_module_specifiers(source, "comments.ts"),
            vec!["./real-module"]
        );
    }

    #[test]
    fn parses_tsx_files_with_jsx_before_imports() {
        let source =
            "const view = <div title=\"[(]\" />;\nimport { helper } from \"./support/helper\";\n";
        assert_eq!(
            extract_relative_module_specifiers(source, "view.tsx"),
            vec!["./support/helper"]
        );
    }
}
