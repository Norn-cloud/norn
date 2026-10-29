//! Workflow YAML as plain data: every scalar is kept as its source string
//! (like `PyYAML`'s `BaseLoader`), so `on`, `true` or `1` are never coerced, and
//! duplicate keys, aliases, tags and non-scalar keys are refused.

use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::Marker;

/// A YAML node with string scalars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A scalar, exactly as written (after YAML unquoting).
    Str(String),
    /// A sequence.
    Seq(Vec<Value>),
    /// A mapping in source order with unique keys.
    Map(Vec<(String, Value)>),
}

impl Value {
    /// Member of a mapping.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            Value::Str(_) | Value::Seq(_) => None,
        }
    }

    /// The scalar, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Seq(_) | Value::Map(_) => None,
        }
    }

    /// Mapping entries (empty for anything else).
    #[must_use]
    pub fn entries(&self) -> &[(String, Value)] {
        match self {
            Value::Map(entries) => entries,
            Value::Str(_) | Value::Seq(_) => &[],
        }
    }

    /// Sequence items (empty for anything else).
    #[must_use]
    pub fn items(&self) -> &[Value] {
        match self {
            Value::Seq(items) => items,
            Value::Str(_) | Value::Map(_) => &[],
        }
    }

    /// True for a null-like scalar (empty, `~`, `null`).
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Str(s) if s.is_empty() || s == "~" || s == "null")
    }

    /// Mapping keys.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries().iter().map(|(k, _)| k.as_str())
    }
}

enum Frame {
    Seq(Vec<Value>),
    Map(Vec<(String, Value)>, Option<String>),
}

#[derive(Default)]
struct Builder {
    stack: Vec<Frame>,
    root: Option<Value>,
    error: Option<String>,
}

impl Builder {
    fn fail(&mut self, message: String) {
        if self.error.is_none() {
            self.error = Some(message);
        }
    }

    fn push(&mut self, value: Value) {
        let mut duplicate = None;
        match self.stack.last_mut() {
            None => self.root = Some(value),
            Some(Frame::Seq(items)) => items.push(value),
            Some(Frame::Map(entries, pending)) => match pending.take() {
                None => match value {
                    Value::Str(key) => {
                        if entries.iter().any(|(k, _)| *k == key) {
                            duplicate = Some(key.clone());
                        }
                        *pending = Some(key);
                    }
                    Value::Seq(_) | Value::Map(_) => {
                        duplicate = Some("<non-scalar key>".to_owned());
                    }
                },
                Some(key) => entries.push((key, value)),
            },
        }
        if let Some(key) = duplicate {
            self.fail(format!("duplicate or non-scalar YAML key: {key}"));
        }
    }
}

impl MarkedEventReceiver for Builder {
    fn on_event(&mut self, event: Event, _mark: Marker) {
        match event {
            Event::Scalar(value, _style, anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.fail("YAML anchors and tags are not allowed".to_owned());
                }
                self.push(Value::Str(value));
            }
            Event::SequenceStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.fail("YAML anchors and tags are not allowed".to_owned());
                }
                self.stack.push(Frame::Seq(Vec::new()));
            }
            Event::MappingStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.fail("YAML anchors and tags are not allowed".to_owned());
                }
                self.stack.push(Frame::Map(Vec::new(), None));
            }
            Event::SequenceEnd => {
                if let Some(Frame::Seq(items)) = self.stack.pop() {
                    self.push(Value::Seq(items));
                }
            }
            Event::MappingEnd => {
                if let Some(Frame::Map(entries, _)) = self.stack.pop() {
                    self.push(Value::Map(entries));
                }
            }
            Event::Alias(_) => self.fail("YAML aliases are not allowed".to_owned()),
            Event::Nothing
            | Event::StreamStart
            | Event::StreamEnd
            | Event::DocumentStart
            | Event::DocumentEnd => {}
        }
    }
}

/// Parse one YAML document into plain data.
///
/// # Errors
/// Returns a message for invalid YAML, duplicate or non-scalar keys,
/// anchors, aliases or tags.
pub fn parse(source: &str) -> Result<Value, String> {
    let mut builder = Builder::default();
    let mut parser = Parser::new_from_str(source);
    parser
        .load(&mut builder, false)
        .map_err(|e| e.to_string())?;
    if let Some(error) = builder.error {
        return Err(error);
    }
    Ok(builder.root.unwrap_or(Value::Map(Vec::new())))
}
