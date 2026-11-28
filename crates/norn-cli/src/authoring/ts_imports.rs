//! Comment- and literal-aware `TypeScript` relative import and re-export specifier scanner.

#[derive(Debug)]
enum Token {
    Ident(String),
    StringLit(String),
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Star,
    Comma,
    Semicolon,
    Dot,
    Other,
}

struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            pos: 0,
        }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_byte_at(&self, offset: usize) -> Option<u8> {
        self.pos
            .checked_add(offset)
            .and_then(|idx| self.bytes.get(idx))
            .copied()
    }

    fn peek_char(&self) -> Option<char> {
        self.source.get(self.pos..)?.chars().next()
    }

    fn advance_ascii_byte(&mut self) {
        if self.pos < self.bytes.len() {
            self.pos = self.pos.saturating_add(1);
        }
    }

    fn next_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos = self.pos.saturating_add(ch.len_utf8());
        Some(ch)
    }

    fn advance_char(&mut self) {
        if let Some(ch) = self.peek_char() {
            self.pos = self.pos.saturating_add(ch.len_utf8());
        }
    }

    fn skip_line_comment(&mut self) {
        self.advance_ascii_byte();
        self.advance_ascii_byte();
        while let Some(ch) = self.peek_char() {
            if ch == '\n' || ch == '\r' {
                break;
            }
            self.advance_char();
        }
    }

    fn skip_block_comment(&mut self) {
        self.advance_ascii_byte();
        self.advance_ascii_byte();
        while let Some(byte) = self.peek_byte() {
            if byte == b'*' && self.peek_byte_at(1) == Some(b'/') {
                self.advance_ascii_byte();
                self.advance_ascii_byte();
                return;
            }
            self.advance_char();
        }
    }

    fn read_string_literal(&mut self, quote: char, mut out: Option<&mut String>) {
        self.advance_ascii_byte();
        while let Some(ch) = self.next_char() {
            if ch == quote || ch == '\n' || ch == '\r' {
                break;
            }
            if ch == '\\' {
                let Some(escaped) = self.next_char() else {
                    break;
                };
                let decoded = match escaped {
                    'n' => Some('\n'),
                    'r' => Some('\r'),
                    't' => Some('\t'),
                    '0' => Some('\0'),
                    '\n' => None,
                    '\r' => {
                        if self.peek_byte() == Some(b'\n') {
                            self.advance_ascii_byte();
                        }
                        None
                    }
                    other => Some(other),
                };
                if let (Some(buf), Some(val)) = (&mut out, decoded) {
                    buf.push(val);
                }
            } else if let Some(buf) = &mut out {
                buf.push(ch);
            }
        }
    }

    fn skip_template_literal(&mut self) {
        self.advance_ascii_byte();
        let mut interp_braces: Vec<usize> = Vec::new();
        let mut in_template = true;
        while let Some(byte) = self.peek_byte() {
            if in_template {
                if byte == b'\\' {
                    self.advance_ascii_byte();
                    self.advance_char();
                    continue;
                }
                if byte == b'`' {
                    self.advance_ascii_byte();
                    if interp_braces.is_empty() {
                        return;
                    }
                    in_template = false;
                    continue;
                }
                if byte == b'$' && self.peek_byte_at(1) == Some(b'{') {
                    self.advance_ascii_byte();
                    self.advance_ascii_byte();
                    interp_braces.push(0);
                    in_template = false;
                    continue;
                }
                self.advance_char();
                continue;
            }
            match byte {
                b'/' if self.peek_byte_at(1) == Some(b'/') => self.skip_line_comment(),
                b'/' if self.peek_byte_at(1) == Some(b'*') => self.skip_block_comment(),
                b'\'' => self.read_string_literal('\'', None),
                b'"' => self.read_string_literal('"', None),
                b'`' => {
                    self.advance_ascii_byte();
                    in_template = true;
                }
                b'{' => {
                    self.advance_ascii_byte();
                    if let Some(depth) = interp_braces.last_mut() {
                        *depth = depth.saturating_add(1);
                    }
                }
                b'}' => {
                    self.advance_ascii_byte();
                    if interp_braces.last().copied() == Some(0) {
                        if interp_braces.pop().is_some() {
                            in_template = true;
                        }
                    } else if let Some(depth) = interp_braces.last_mut() {
                        *depth = depth.saturating_sub(1);
                    }
                }
                _ => self.advance_char(),
            }
        }
    }

    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(ch) = self.peek_char() {
            if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                self.advance_char();
            } else {
                break;
            }
        }
        self.source.get(start..self.pos).unwrap_or("").to_owned()
    }

    fn tokenize(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                self.advance_char();
                continue;
            }
            match ch {
                '/' if self.peek_byte_at(1) == Some(b'/') => self.skip_line_comment(),
                '/' if self.peek_byte_at(1) == Some(b'*') => self.skip_block_comment(),
                '\'' | '"' => {
                    let mut text = String::new();
                    self.read_string_literal(ch, Some(&mut text));
                    tokens.push(Token::StringLit(text));
                }
                '`' => {
                    self.skip_template_literal();
                    tokens.push(Token::Other);
                }
                '{' => {
                    self.advance_ascii_byte();
                    tokens.push(Token::LBrace);
                }
                '}' => {
                    self.advance_ascii_byte();
                    tokens.push(Token::RBrace);
                }
                '(' => {
                    self.advance_ascii_byte();
                    tokens.push(Token::LParen);
                }
                ')' => {
                    self.advance_ascii_byte();
                    tokens.push(Token::RParen);
                }
                '[' => {
                    self.advance_ascii_byte();
                    tokens.push(Token::LBracket);
                }
                ']' => {
                    self.advance_ascii_byte();
                    tokens.push(Token::RBracket);
                }
                '*' => {
                    self.advance_ascii_byte();
                    tokens.push(Token::Star);
                }
                ',' => {
                    self.advance_ascii_byte();
                    tokens.push(Token::Comma);
                }
                ';' => {
                    self.advance_ascii_byte();
                    tokens.push(Token::Semicolon);
                }
                '.' => {
                    self.advance_ascii_byte();
                    tokens.push(Token::Dot);
                }
                _ if is_ident_start(ch) => tokens.push(Token::Ident(self.read_ident())),
                _ => {
                    self.advance_char();
                    tokens.push(Token::Other);
                }
            }
        }
        tokens
    }
}

fn is_ident_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_' || ch == '$'
}

fn skip_brace_block(tokens: &[Token], start: usize) -> Option<usize> {
    if !matches!(tokens.get(start), Some(Token::LBrace)) {
        return None;
    }
    let mut depth: usize = 0;
    let mut idx = start;
    while let Some(tok) = tokens.get(idx) {
        match tok {
            Token::LBrace => depth = depth.saturating_add(1),
            Token::RBrace => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(idx.saturating_add(1));
                }
            }
            Token::Ident(_)
            | Token::StringLit(_)
            | Token::LParen
            | Token::RParen
            | Token::LBracket
            | Token::RBracket
            | Token::Star
            | Token::Comma
            | Token::Semicolon
            | Token::Dot
            | Token::Other => {}
        }
        idx = idx.saturating_add(1);
    }
    None
}

fn match_from_specifier(tokens: &[Token], idx: usize) -> Option<(&str, usize)> {
    let Some(Token::Ident(word)) = tokens.get(idx) else {
        return None;
    };
    if word != "from" {
        return None;
    }
    let next = idx.saturating_add(1);
    let Some(Token::StringLit(spec)) = tokens.get(next) else {
        return None;
    };
    Some((spec.as_str(), next.saturating_add(1)))
}

fn parse_import_specifier(tokens: &[Token], start: usize) -> Option<(&str, usize)> {
    match tokens.get(start)? {
        Token::StringLit(spec) => return Some((spec.as_str(), start.saturating_add(1))),
        Token::LParen | Token::Dot => return None,
        Token::Ident(_)
        | Token::LBrace
        | Token::RBrace
        | Token::RParen
        | Token::LBracket
        | Token::RBracket
        | Token::Star
        | Token::Comma
        | Token::Semicolon
        | Token::Other => {}
    }

    let mut idx = start;
    while let Some(tok) = tokens.get(idx) {
        if let Some(found) = match_from_specifier(tokens, idx) {
            return Some(found);
        }
        match tok {
            Token::LBrace => {
                idx = skip_brace_block(tokens, idx)?;
            }
            Token::Ident(_) | Token::Star | Token::Comma => {
                idx = idx.saturating_add(1);
            }
            Token::StringLit(_)
            | Token::RBrace
            | Token::LParen
            | Token::RParen
            | Token::LBracket
            | Token::RBracket
            | Token::Semicolon
            | Token::Dot
            | Token::Other => return None,
        }
    }
    None
}

fn parse_export_specifier(tokens: &[Token], start: usize) -> Option<(&str, usize)> {
    let mut idx = start;
    if let Some(Token::Ident(word)) = tokens.get(idx)
        && word == "type"
    {
        idx = idx.saturating_add(1);
    }
    match tokens.get(idx)? {
        Token::LBrace => {
            let after_braces = skip_brace_block(tokens, idx)?;
            match_from_specifier(tokens, after_braces)
        }
        Token::Star => {
            let mut after_star = idx.saturating_add(1);
            if let Some(Token::Ident(word)) = tokens.get(after_star)
                && word == "as"
                && matches!(
                    tokens.get(after_star.saturating_add(1)),
                    Some(Token::Ident(_) | Token::StringLit(_))
                )
            {
                after_star = after_star.saturating_add(2);
            }
            match_from_specifier(tokens, after_star)
        }
        Token::Ident(_)
        | Token::StringLit(_)
        | Token::RBrace
        | Token::LParen
        | Token::RParen
        | Token::LBracket
        | Token::RBracket
        | Token::Comma
        | Token::Semicolon
        | Token::Dot
        | Token::Other => None,
    }
}

fn push_relative_specifier(specs: &mut Vec<String>, spec: &str) {
    if spec.starts_with("./") || spec.starts_with("../") {
        specs.push(spec.to_owned());
    }
}

/// Extracts relative (`./` and `../`) static module specifiers from `TypeScript`
/// `import` and re-export `export ... from` declarations, skipping comments and
/// string or template literals.
pub(crate) fn extract_relative_specifiers(source: &str) -> Vec<String> {
    let tokens = Lexer::new(source).tokenize();
    let mut specs = Vec::new();
    let mut brace_depth: usize = 0;
    let mut paren_depth: usize = 0;
    let mut bracket_depth: usize = 0;
    let mut prev_is_dot = false;
    let mut idx: usize = 0;

    while let Some(tok) = tokens.get(idx) {
        let at_top_level = brace_depth == 0 && paren_depth == 0 && bracket_depth == 0;
        if at_top_level && !prev_is_dot {
            if let Token::Ident(word) = tok
                && word == "import"
                && let Some((spec, next_idx)) =
                    parse_import_specifier(&tokens, idx.saturating_add(1))
            {
                push_relative_specifier(&mut specs, spec);
                idx = next_idx;
                prev_is_dot = false;
                continue;
            }
            if let Token::Ident(word) = tok
                && word == "export"
                && let Some((spec, next_idx)) =
                    parse_export_specifier(&tokens, idx.saturating_add(1))
            {
                push_relative_specifier(&mut specs, spec);
                idx = next_idx;
                prev_is_dot = false;
                continue;
            }
        }
        match tok {
            Token::LBrace => brace_depth = brace_depth.saturating_add(1),
            Token::RBrace => brace_depth = brace_depth.saturating_sub(1),
            Token::LParen => paren_depth = paren_depth.saturating_add(1),
            Token::RParen => paren_depth = paren_depth.saturating_sub(1),
            Token::LBracket => bracket_depth = bracket_depth.saturating_add(1),
            Token::RBracket => bracket_depth = bracket_depth.saturating_sub(1),
            Token::Ident(_)
            | Token::StringLit(_)
            | Token::Star
            | Token::Comma
            | Token::Semicolon
            | Token::Dot
            | Token::Other => {}
        }
        prev_is_dot = matches!(tok, Token::Dot);
        idx = idx.saturating_add(1);
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::extract_relative_specifiers;

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
            extract_relative_specifiers(source),
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
        assert_eq!(extract_relative_specifiers(source), vec!["./real-module"]);
    }
}
