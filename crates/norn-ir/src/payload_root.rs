//! The decode error of an artifact payload root.

/// The wire payload root must be a JSON object before it can select a closed kind root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadRootDecodeError;

impl std::fmt::Display for PayloadRootDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("artifact payload must be a JSON object")
    }
}

impl std::error::Error for PayloadRootDecodeError {}
