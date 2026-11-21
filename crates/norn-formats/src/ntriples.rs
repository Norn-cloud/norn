//! A bounded N-Triples reader (RDF 1.1 N-Triples): the input form of the OWL
//! reader. Any RDF serialization reaches it through a pinned normaliser that
//! writes canonical, sorted N-Triples, so this reader only has to read one
//! line-based grammar: no XML, no DTD, no entity expansion, no network.
//!
//! Hostile input is refused with a message, never a panic: more than
//! [`MAX_TRIPLES`] statements, a line over [`MAX_LINE_BYTES`], a relative or
//! malformed IRI, a bad escape, or trailing content after the final `.`.

use std::fmt;

/// The most statements one graph may hold.
pub const MAX_TRIPLES: usize = 2_000_000;
/// The longest admitted line, in bytes.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// One RDF term.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Term {
    /// An absolute IRI.
    Iri(String),
    /// A blank node label (without `_:`).
    Blank(String),
    /// A literal: lexical form, datatype IRI, and language tag (for
    /// `rdf:langString`).
    Literal {
        /// The lexical form, unescaped.
        value: String,
        /// The datatype IRI (`xsd:string` when none was written).
        datatype: String,
        /// The language tag, lower-cased.
        language: Option<String>,
    },
}

/// `xsd:string`.
pub const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
/// `rdf:langString`.
pub const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";

impl Term {
    /// The IRI, when the term is one.
    #[must_use]
    pub fn iri(&self) -> Option<&str> {
        match self {
            Self::Iri(iri) => Some(iri),
            Self::Blank(_) | Self::Literal { .. } => None,
        }
    }

    /// Whether the term is a blank node.
    #[must_use]
    pub const fn is_blank(&self) -> bool {
        matches!(self, Self::Blank(_))
    }
}

impl fmt::Display for Term {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Iri(iri) => write!(formatter, "<{iri}>"),
            Self::Blank(label) => write!(formatter, "_:{label}"),
            Self::Literal {
                value,
                datatype,
                language,
            } => {
                write!(formatter, "{value:?}")?;
                match language {
                    Some(language) => write!(formatter, "@{language}"),
                    None if datatype == XSD_STRING => Ok(()),
                    None => write!(formatter, "^^<{datatype}>"),
                }
            }
        }
    }
}

/// One statement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Triple {
    /// An IRI or blank node.
    pub subject: Term,
    /// An IRI.
    pub predicate: String,
    /// Any term.
    pub object: Term,
}

/// Parse an N-Triples document. Comments and blank lines are skipped;
/// duplicate statements are kept once.
///
/// # Errors
///
/// A message naming the 1-based line of the first defect.
pub fn parse(bytes: &[u8]) -> Result<Vec<Triple>, String> {
    let text = std::str::from_utf8(bytes).map_err(|error| format!("not UTF-8: {error}"))?;
    let mut triples = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index.saturating_add(1);
        if line.len() > MAX_LINE_BYTES {
            return Err(format!("line {number}: longer than {MAX_LINE_BYTES} bytes"));
        }
        let mut cursor = Cursor::new(line);
        cursor.skip_space();
        if cursor.done() || cursor.peek() == Some('#') {
            continue;
        }
        let triple = cursor
            .triple()
            .map_err(|error| format!("line {number}: {error}"))?;
        triples.push(triple);
        if triples.len() > MAX_TRIPLES {
            return Err(format!("more than {MAX_TRIPLES} statements"));
        }
    }
    triples.sort();
    triples.dedup();
    Ok(triples)
}

struct Cursor<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Cursor<'a> {
    fn new(line: &'a str) -> Self {
        Self {
            chars: line.chars().peekable(),
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn done(&mut self) -> bool {
        self.peek().is_none()
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.chars.next();
        }
    }

    fn expect(&mut self, wanted: char) -> Result<(), String> {
        match self.chars.next() {
            Some(found) if found == wanted => Ok(()),
            Some(found) => Err(format!("expected {wanted:?}, found {found:?}")),
            None => Err(format!("expected {wanted:?}, found end of line")),
        }
    }

    fn triple(&mut self) -> Result<Triple, String> {
        let subject = match self.peek() {
            Some('<') => Term::Iri(self.iri()?),
            Some('_') => Term::Blank(self.blank()?),
            _ => return Err("a subject is an IRI or a blank node".into()),
        };
        self.skip_space();
        if self.peek() != Some('<') {
            return Err("a predicate is an IRI".into());
        }
        let predicate = self.iri()?;
        self.skip_space();
        let object = match self.peek() {
            Some('<') => Term::Iri(self.iri()?),
            Some('_') => Term::Blank(self.blank()?),
            Some('"') => self.literal()?,
            _ => return Err("an object is an IRI, a blank node or a literal".into()),
        };
        self.skip_space();
        self.expect('.')?;
        self.skip_space();
        match self.peek() {
            None | Some('#') => Ok(Triple {
                subject,
                predicate,
                object,
            }),
            Some(found) => Err(format!("unexpected {found:?} after the final '.'")),
        }
    }

    fn iri(&mut self) -> Result<String, String> {
        self.expect('<')?;
        let mut iri = String::new();
        loop {
            match self.chars.next() {
                Some('>') => break,
                Some('\\') => iri.push(self.unicode_escape()?),
                Some(c) if c <= ' ' || matches!(c, '<' | '"' | '{' | '}' | '|' | '^' | '`') => {
                    return Err(format!("character {c:?} is not allowed in an IRI"));
                }
                Some(c) => iri.push(c),
                None => return Err("unterminated IRI".into()),
            }
        }
        let scheme = iri.split_once(':').map(|(scheme, _)| scheme);
        if !scheme.is_some_and(|scheme| {
            scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }) {
            return Err(format!("IRI <{iri}> is not absolute"));
        }
        Ok(iri)
    }

    fn blank(&mut self) -> Result<String, String> {
        self.expect('_')?;
        self.expect(':')?;
        let mut label = String::new();
        while let Some(c) = self.peek() {
            // '.' is left out of labels (the grammar allows it inside one) so a
            // label can never swallow the statement terminator.
            if c.is_alphanumeric() || matches!(c, '_' | '-') {
                label.push(c);
                self.chars.next();
            } else {
                break;
            }
        }
        if label.is_empty() {
            return Err("empty blank node label".into());
        }
        Ok(label)
    }

    fn literal(&mut self) -> Result<Term, String> {
        self.expect('"')?;
        let mut value = String::new();
        loop {
            match self.chars.next() {
                Some('"') => break,
                Some('\\') => value.push(self.escape()?),
                Some('\n' | '\r') => return Err("raw line break in a literal".into()),
                Some(c) => value.push(c),
                None => return Err("unterminated literal".into()),
            }
        }
        match self.peek() {
            Some('@') => {
                self.chars.next();
                let mut language = String::new();
                while let Some(c) = self.peek() {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        language.push(c.to_ascii_lowercase());
                        self.chars.next();
                    } else {
                        break;
                    }
                }
                if language.is_empty() {
                    return Err("empty language tag".into());
                }
                Ok(Term::Literal {
                    value,
                    datatype: RDF_LANG_STRING.to_owned(),
                    language: Some(language),
                })
            }
            Some('^') => {
                self.expect('^')?;
                self.expect('^')?;
                Ok(Term::Literal {
                    value,
                    datatype: self.iri()?,
                    language: None,
                })
            }
            _ => Ok(Term::Literal {
                value,
                datatype: XSD_STRING.to_owned(),
                language: None,
            }),
        }
    }

    fn escape(&mut self) -> Result<char, String> {
        match self.peek() {
            Some('u' | 'U') => self.unicode_escape(),
            _ => match self.chars.next() {
                Some('t') => Ok('\t'),
                Some('b') => Ok('\u{8}'),
                Some('n') => Ok('\n'),
                Some('r') => Ok('\r'),
                Some('f') => Ok('\u{c}'),
                Some('"') => Ok('"'),
                Some('\'') => Ok('\''),
                Some('\\') => Ok('\\'),
                Some(other) => Err(format!("unknown escape \\{other}")),
                None => Err("unterminated escape".into()),
            },
        }
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let digits = match self.chars.next() {
            Some('u') => 4,
            Some('U') => 8,
            Some(other) => return Err(format!("unknown escape \\{other}")),
            None => return Err("unterminated escape".into()),
        };
        let mut code = 0_u32;
        for _ in 0..digits {
            let digit = self
                .chars
                .next()
                .and_then(|c| c.to_digit(16))
                .ok_or("bad hexadecimal escape")?;
            code = code
                .checked_mul(16)
                .and_then(|code| code.checked_add(digit))
                .ok_or("escape out of range")?;
        }
        char::from_u32(code).ok_or_else(|| format!("U+{code:X} is not a character"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_parse_with_escapes_languages_and_datatypes() {
        let text = concat!(
            "# a comment\n",
            "<https://e.x/A> <http://www.w3.org/2000/01/rdf-schema#label> \"A \\\"quoted\\\" \\u00e9\"@EN .\n",
            "\n",
            "_:b0 <https://e.x/p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n",
            "<https://e.x/A> <https://e.x/q> _:b0 .\n",
            "<https://e.x/A> <https://e.x/q> _:b0 .\n",
        );
        let triples = parse(text.as_bytes()).expect("parses");
        assert_eq!(triples.len(), 3, "duplicates collapse");
        assert!(triples.iter().any(|t| t.object
            == Term::Literal {
                value: "A \"quoted\" é".into(),
                datatype: RDF_LANG_STRING.into(),
                language: Some("en".into()),
            }));
        assert!(triples.iter().any(|t| t.subject == Term::Blank("b0".into())
            && t.object
                == Term::Literal {
                    value: "1".into(),
                    datatype: "http://www.w3.org/2001/XMLSchema#integer".into(),
                    language: None,
                }));
    }

    #[test]
    fn malformed_lines_are_refused_with_their_line_number() {
        for (text, needle) in [
            ("<rel> <https://e.x/p> <https://e.x/o> .\n", "not absolute"),
            ("<https://e.x/s> <https://e.x/p> \"open .\n", "unterminated"),
            (
                "<https://e.x/s> <https://e.x/p> <https://e.x/o>\n",
                "expected '.'",
            ),
            (
                "<https://e.x/s> <https://e.x/p> <https://e.x/o> . x\n",
                "after the final",
            ),
            ("\"lit\" <https://e.x/p> <https://e.x/o> .\n", "subject"),
            (
                "<https://e.x/s> <https://e.x/p> \"\\q\" .\n",
                "unknown escape",
            ),
        ] {
            let error = parse(text.as_bytes()).expect_err(text);
            assert!(error.starts_with("line 1:"), "{error}");
            assert!(error.contains(needle), "{error} lacks {needle}");
        }
    }
}
