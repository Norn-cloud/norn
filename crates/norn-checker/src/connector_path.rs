//! Parsed JSON value paths shared by connector admission and execution.

use std::fmt;

use serde_json::Value;

/// A parsed connector value path.
///
/// JSON Pointers are evaluated by `serde_json`; simple paths accept dot
/// separated object keys and at most one terminal `[index]` per component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorValuePath {
    kind: PathKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathKind {
    Root,
    JsonPointer(String),
    Simple(Vec<PathComponent>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathComponent {
    key: Option<String>,
    index: Option<u64>,
}

/// A syntax error in a connector value path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorValuePathError {
    /// The path or a simple-path component has leading or trailing whitespace.
    WhitespacePadding,
    /// The path contains a control character.
    ControlCharacter,
    /// A dot-separated path has an empty component.
    EmptyComponent,
    /// A JSON Pointer contains a `~` not followed by `0` or `1`.
    InvalidPointerEscape,
    /// An array index has unmatched brackets, trailing content, or multiple indexes.
    MalformedArrayIndex,
    /// An array index is not unsigned ASCII digits or does not fit in `u64`.
    InvalidArrayIndex,
}

impl fmt::Display for ConnectorValuePathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WhitespacePadding => "path or path component has leading or trailing whitespace",
            Self::ControlCharacter => "path contains a control character",
            Self::EmptyComponent => "dot-separated path contains an empty component",
            Self::InvalidPointerEscape => "JSON Pointer '~' must be followed by '0' or '1'",
            Self::MalformedArrayIndex => {
                "array index must be one terminal [digits] suffix per component"
            }
            Self::InvalidArrayIndex => "array index must be unsigned ASCII digits that fit u64",
        })
    }
}

impl std::error::Error for ConnectorValuePathError {}

impl ConnectorValuePath {
    /// Parse a connector path without resolving it against a particular value.
    ///
    /// The empty string and `$` select the root for runtime compatibility.
    /// Callers that require a member path must reject those two root forms
    /// separately.
    ///
    /// # Errors
    /// Returns an error when the path has invalid whitespace, control
    /// characters, pointer escapes, dot components, or array-index syntax.
    pub fn parse(path: &str) -> Result<Self, ConnectorValuePathError> {
        if path.chars().any(char::is_control) {
            return Err(ConnectorValuePathError::ControlCharacter);
        }
        if path.trim() != path {
            return Err(ConnectorValuePathError::WhitespacePadding);
        }
        if path.is_empty() || path == "$" {
            return Ok(Self {
                kind: PathKind::Root,
            });
        }
        if path.starts_with('/') {
            validate_pointer(path)?;
            return Ok(Self {
                kind: PathKind::JsonPointer(path.to_owned()),
            });
        }

        let path = path.strip_prefix("$.").unwrap_or(path);
        let mut components = Vec::new();
        for component in path.split('.') {
            if component.is_empty() {
                return Err(ConnectorValuePathError::EmptyComponent);
            }
            if component.trim() != component {
                return Err(ConnectorValuePathError::WhitespacePadding);
            }
            components.push(parse_component(component)?);
        }
        Ok(Self {
            kind: PathKind::Simple(components),
        })
    }

    /// Resolve this path against a JSON value.
    #[must_use]
    pub fn get<'value>(&self, value: &'value Value) -> Option<&'value Value> {
        match &self.kind {
            PathKind::Root => Some(value),
            PathKind::JsonPointer(pointer) => value.pointer(pointer),
            PathKind::Simple(components) => {
                let mut current = value;
                for component in components {
                    if let Some(key) = &component.key {
                        current = current.get(key)?;
                    }
                    if let Some(index) = component.index {
                        current = current.get(usize::try_from(index).ok()?)?;
                    }
                }
                Some(current)
            }
        }
    }
}

fn validate_pointer(pointer: &str) -> Result<(), ConnectorValuePathError> {
    for component in pointer.split('/').skip(1) {
        let mut characters = component.chars();
        while let Some(character) = characters.next() {
            if character == '~' && !matches!(characters.next(), Some('0' | '1')) {
                return Err(ConnectorValuePathError::InvalidPointerEscape);
            }
        }
    }
    Ok(())
}

fn parse_component(component: &str) -> Result<PathComponent, ConnectorValuePathError> {
    let Some(open) = component.find('[') else {
        if component.contains(']') {
            return Err(ConnectorValuePathError::MalformedArrayIndex);
        }
        return Ok(PathComponent {
            key: Some(component.to_owned()),
            index: None,
        });
    };

    let suffix = component
        .get(open..)
        .ok_or(ConnectorValuePathError::MalformedArrayIndex)?;
    let key = component
        .get(..open)
        .ok_or(ConnectorValuePathError::MalformedArrayIndex)?;
    if key.contains(']') {
        return Err(ConnectorValuePathError::MalformedArrayIndex);
    }
    let digits = suffix
        .strip_prefix('[')
        .and_then(|suffix| suffix.strip_suffix(']'))
        .ok_or(ConnectorValuePathError::MalformedArrayIndex)?;
    if digits
        .chars()
        .any(|character| matches!(character, '[' | ']'))
    {
        return Err(ConnectorValuePathError::MalformedArrayIndex);
    }
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ConnectorValuePathError::InvalidArrayIndex);
    }
    let index = digits
        .parse::<u64>()
        .map_err(|_error| ConnectorValuePathError::InvalidArrayIndex)?;
    let key = (!key.is_empty()).then(|| key.to_owned());
    Ok(PathComponent {
        key,
        index: Some(index),
    })
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "Focused parser unit tests use explicit expectations for fixture readability"
)]
mod tests {
    use super::ConnectorValuePath;
    use serde_json::json;

    #[test]
    fn poll_connector_path_resolves_simple_dot_and_array_segments() {
        let value = json!({"data": {"rows": [{"id": "row-1"}]}});
        for path in ["data.rows[0].id", "$.data.rows[0].id"] {
            assert_eq!(
                ConnectorValuePath::parse(path)
                    .expect("valid simple path")
                    .get(&value),
                Some(&json!("row-1"))
            );
        }
    }

    #[test]
    fn poll_connector_path_preserves_root_and_root_array_index_paths() {
        let value = json!(["first", "second"]);
        for path in ["", "$"] {
            assert_eq!(
                ConnectorValuePath::parse(path)
                    .expect("valid root path")
                    .get(&value),
                Some(&value)
            );
        }
        assert_eq!(
            ConnectorValuePath::parse("[0]")
                .expect("valid root array index")
                .get(&value),
            Some(&json!("first"))
        );

        // Admission must use the same index width on native and wasm hosts.
        let large_index = ConnectorValuePath::parse("[4294967296]")
            .expect("index syntax is independent of pointer width");
        assert_eq!(large_index.get(&value), None);
    }

    #[test]
    fn poll_connector_path_preserves_pointer_escapes_and_numeric_object_keys() {
        let value = json!({"a/b": {"~field": {"0": "numeric object key"}}});
        let path = ConnectorValuePath::parse("/a~1b/~0field/0").expect("valid pointer");
        assert_eq!(path.get(&value), Some(&json!("numeric object key")));

        let value = json!({"~1": "escaped in RFC order"});
        assert_eq!(
            ConnectorValuePath::parse("/~01")
                .expect("valid two-step pointer escape")
                .get(&value),
            Some(&json!("escaped in RFC order"))
        );

        let value = json!({" spaced key ": {"id": "whitespace inside a pointer token"}});
        assert_eq!(
            ConnectorValuePath::parse("/ spaced key /id")
                .expect("valid pointer member containing whitespace")
                .get(&value),
            Some(&json!("whitespace inside a pointer token"))
        );
    }

    #[test]
    fn poll_connector_path_rejects_invalid_pointer_escapes_and_indexes() {
        for path in ["/a~", "/a~2", "a..b", "a[1][2]", "a[1", "a[-1]"] {
            assert!(
                ConnectorValuePath::parse(path).is_err(),
                "accepted {path:?}"
            );
        }
    }

    #[test]
    fn poll_connector_path_keeps_pointer_array_index_and_missing_value_semantics() {
        let value = json!({"rows": ["first"]});
        for path in ["/rows/00", "/rows/+0", "/rows/1", "/missing"] {
            assert_eq!(
                ConnectorValuePath::parse(path)
                    .expect("valid pointer syntax")
                    .get(&value),
                None
            );
        }
    }
}
