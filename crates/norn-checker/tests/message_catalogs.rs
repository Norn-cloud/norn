//! MESSAGE-LOCALE-1: stable codes map to ICU MessageFormat 2 messages per
//! locale (`conformance/messages/<locale>.json`). The code is the contract;
//! a catalog only says it in a language. The English catalog is generated
//! from the code registry (`NORN_BLESS=1` rewrites it); every other catalog
//! covers every registered code with the same placeholders, and its Arabic
//! terms follow the Egyptian business glossary.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "catalog conformance tests"
)]
use norn_checker::codes::REGISTRY;
use proptest::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const LOCALES: &[&str] = &["ar"];

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Catalog {
    format: String,
    locale: String,
    messages: BTreeMap<String, Entry>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Entry {
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review: Option<Review>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Review {
    NeedsReview,
    Reviewed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Glossary {
    format: String,
    source: String,
    locale: String,
    terms: Vec<Term>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Term {
    en: String,
    ar: Vec<String>,
    domain: String,
}

fn directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("conformance/messages")
}

fn read<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    serde_json::from_str(&std::fs::read_to_string(directory().join(name)).expect("catalog file"))
        .expect("catalog JSON")
}

/// One piece of a parsed simple MessageFormat 2 message.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    Variable(String),
}

/// Parse the simple-message subset the catalogs use: text with `\\`, `\{`
/// and `\}` escapes, and `{$name}` variable placeholders. Anything else
/// (functions, markup, declarations, a bare brace) is refused.
fn parse(message: &str) -> Result<Vec<Part>, String> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut chars = message.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(escaped @ ('\\' | '{' | '}')) => text.push(escaped),
                other => return Err(format!("invalid escape {other:?}")),
            },
            '{' => {
                let body: String = chars.by_ref().take_while(|c| *c != '}').collect();
                let name = body
                    .strip_prefix('$')
                    .filter(|name| {
                        name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    })
                    .ok_or_else(|| format!("unsupported expression {{{body}}}"))?;
                if !text.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut text)));
                }
                parts.push(Part::Variable(name.to_owned()));
            }
            '}' => return Err("unescaped }".into()),
            other => text.push(other),
        }
    }
    if !text.is_empty() {
        parts.push(Part::Text(text));
    }
    Ok(parts)
}

/// The registry's `{placeholder}` shape as MessageFormat 2: an identifier in
/// braces becomes the variable `{$name}`; every other brace, backslash and
/// braced text stays literal, escaped.
fn mf2(shape: &str) -> String {
    let escape = |text: &str| {
        text.replace('\\', "\\\\")
            .replace('{', "\\{")
            .replace('}', "\\}")
    };
    let identifier = |body: &str| {
        body.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && body.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let mut out = String::new();
    let mut rest = shape;
    while let Some(open) = rest.find('{') {
        let body_and_rest = &rest[open + 1..];
        match body_and_rest.find(['{', '}']) {
            Some(close) if body_and_rest[close..].starts_with('}') => {
                let body = &body_and_rest[..close];
                out.push_str(&escape(&rest[..open]));
                if identifier(body) {
                    out.push_str(&format!("{{${body}}}"));
                } else {
                    out.push_str(&escape(&rest[open..open + close + 2]));
                }
                rest = &body_and_rest[close + 1..];
            }
            _ => {
                out.push_str(&escape(&rest[..=open]));
                rest = body_and_rest;
            }
        }
    }
    out.push_str(&escape(rest));
    out
}

fn variables(message: &str) -> BTreeSet<String> {
    parse(message)
        .expect("well-formed message")
        .into_iter()
        .filter_map(|part| match part {
            Part::Variable(name) => Some(name),
            Part::Text(_) => None,
        })
        .collect()
}

fn english() -> Catalog {
    Catalog {
        format: "norn.messages/1".into(),
        locale: "en".into(),
        messages: REGISTRY
            .iter()
            .map(|entry| {
                (
                    entry.code.to_owned(),
                    Entry {
                        message: mf2(entry.message),
                        review: None,
                    },
                )
            })
            .collect(),
    }
}

#[test]
fn the_english_catalog_is_the_registry() {
    let generated = english();
    let path = directory().join("en.json");
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&generated).expect("catalog JSON")
    );
    if std::env::var("NORN_BLESS").as_deref() == Ok("1") {
        std::fs::write(&path, &text).expect("bless the English catalog");
    }
    assert!(
        std::fs::read_to_string(&path).expect("en.json") == text,
        "en.json differs from the code registry; rerun with NORN_BLESS=1 and review the diff"
    );
}

#[test]
fn every_catalog_covers_every_code_with_its_placeholders() {
    let english = english();
    for locale in LOCALES {
        let catalog: Catalog = read(&format!("{locale}.json"));
        assert_eq!(catalog.format, "norn.messages/1");
        assert_eq!(&catalog.locale, locale);
        let codes: BTreeSet<_> = english.messages.keys().collect();
        let translated: BTreeSet<_> = catalog.messages.keys().collect();
        assert_eq!(
            codes.difference(&translated).collect::<Vec<_>>(),
            Vec::<&&String>::new(),
            "{locale}: untranslated codes"
        );
        assert_eq!(
            translated.difference(&codes).collect::<Vec<_>>(),
            Vec::<&&String>::new(),
            "{locale}: codes the registry does not declare"
        );
        for (code, entry) in &catalog.messages {
            let source = &english.messages[code].message;
            assert_eq!(
                variables(&entry.message),
                variables(source),
                "{locale} {code}: placeholders differ from the English message"
            );
            assert_eq!(
                norn_canonical::nfc(&entry.message),
                entry.message,
                "{locale} {code}: not NFC"
            );
            assert!(
                !entry
                    .message
                    .chars()
                    .any(|c| matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
                "{locale} {code}: bidi embedding, override or isolate control"
            );
            assert!(
                !entry.message.trim().is_empty(),
                "{locale} {code}: blank message"
            );
        }
    }
}

/// Strip harakat and tatweel, and the definite article with its common
/// proclitics, so a glossary term matches its inflected uses.
fn arabic_stem(text: &str) -> String {
    let bare: String = text
        .chars()
        .filter(|c| !matches!(c, '\u{064b}'..='\u{0652}' | '\u{0640}'))
        .collect();
    bare.split_whitespace()
        .map(|word| {
            ["وال", "بال", "فال", "كال", "لل", "ال"]
                .iter()
                .find_map(|prefix| word.strip_prefix(prefix))
                .unwrap_or(word)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The English words a message says, without placeholders, quoted or
/// bracketed literals and camelCase identifiers.
fn english_prose(message: &str) -> String {
    let mut prose = String::new();
    let mut skip: Option<char> = None;
    for c in message.chars() {
        match (skip, c) {
            (Some(end), c) if c == end => skip = None,
            (Some(_), _) => {}
            (None, '{') => skip = Some('}'),
            (None, '`') => skip = Some('`'),
            (None, '"') => skip = Some('"'),
            (None, '[') => skip = Some(']'),
            (None, c) => prose.push(c),
        }
    }
    prose
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .filter(|word| !word.chars().skip(1).any(|c| c.is_ascii_uppercase()) && !word.contains('-'))
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn says(prose: &str, term: &str) -> bool {
    let words: Vec<&str> = prose.split(' ').filter(|w| !w.is_empty()).collect();
    let term: Vec<&str> = term.split(' ').collect();
    words.windows(term.len()).any(|window| {
        window
            .iter()
            .zip(&term)
            .enumerate()
            .all(|(index, (word, term))| {
                *word == *term
                    || (index + 1 == term.len()
                        && (word.strip_suffix('s') == Some(term)
                            || word.strip_suffix("es") == Some(term)))
            })
    })
}

#[test]
fn arabic_messages_use_the_glossary_terms() {
    let glossary: Glossary = read("glossary.ar-EG.json");
    assert_eq!(
        (
            glossary.format.as_str(),
            glossary.source.as_str(),
            glossary.locale.as_str()
        ),
        ("norn.glossary/1", "en", "ar-EG")
    );
    let english = english();
    let arabic: Catalog = read("ar.json");
    let mut misses = Vec::new();
    for term in &glossary.terms {
        assert!(matches!(term.domain.as_str(), "business" | "norn"));
        assert!(!term.ar.is_empty(), "{} has no Arabic form", term.en);
        for (code, entry) in &english.messages {
            if says(&english_prose(&entry.message), &term.en) {
                let translated = arabic_stem(&arabic.messages[code].message);
                if !term
                    .ar
                    .iter()
                    .any(|form| translated.contains(&arabic_stem(form)))
                {
                    misses.push(format!("{code}: {} → {:?}", term.en, term.ar));
                }
            }
        }
    }
    assert!(
        misses.is_empty(),
        "glossary terms not used:\n{}",
        misses.join("\n")
    );
}

proptest! {
    /// Any registry shape converts to a message that parses back to exactly
    /// its text and identifier placeholders: escaping never loses a character.
    #[test]
    fn registry_shapes_round_trip_through_messageformat(
        pieces in prop::collection::vec(
            prop_oneof![
                "[a-zA-Z ,.:;()\\[\\]`\"'\\\\<>=_-]{0,8}".prop_map(|text| (false, text)),
                "[a-z][a-zA-Z0-9_]{0,6}".prop_map(|name| (true, name)),
                "[a-z]{1,4}: [a-z]{1,4}".prop_map(|body| (false, format!("{{{body}}}"))),
            ],
            0..8,
        )
    ) {
        let shape: String = pieces
            .iter()
            .map(|(variable, text)| if *variable { format!("{{{text}}}") } else { text.clone() })
            .collect();
        let parsed = parse(&mf2(&shape)).expect("generated message parses");
        let rendered: String = parsed
            .iter()
            .map(|part| match part {
                Part::Text(text) => text.clone(),
                Part::Variable(name) => format!("{{{name}}}"),
            })
            .collect();
        prop_assert_eq!(rendered, shape);
    }

    /// The parser refuses every expression form outside the simple subset.
    #[test]
    fn non_variable_expressions_are_refused(body in "[a-z :|*]{1,8}") {
        prop_assume!(!body.starts_with('$'));
        let message = format!("x {{{body}}} y");
        prop_assert!(parse(&message).is_err());
    }
}
