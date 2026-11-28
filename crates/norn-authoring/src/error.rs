use thiserror::Error;

/// Errors returned by the Norn TypeScript authoring compiler and formatter.
#[derive(Debug, Error)]
pub enum AuthoringError {
    /// The embedded authoring profile JSON could not be parsed or failed structural validation.
    #[error("invalid authoring profile: {0}")]
    InvalidProfile(String),

    /// A virtual filesystem quota (file count or byte budget) was exceeded.
    #[error("virtual filesystem quota exceeded: {0}")]
    VfsQuotaExceeded(String),

    /// A virtual filesystem path was malformed or attempted directory traversal.
    #[error("invalid virtual path `{path}`: {reason}")]
    InvalidPath {
        /// The rejected virtual path.
        path: String,
        /// Why the path was rejected.
        reason: String,
    },

    /// A requested source file was not loaded in the compiled program.
    #[error("source file not found in program: {0}")]
    SourceFileNotFound(String),

    /// Formatting or non-destructive import organization failed.
    #[error("formatting failed: {0}")]
    FormatFailed(String),

    /// A text edit range was out of bounds or not aligned to `UTF-8` character boundaries.
    #[error("invalid text edit range [{start}..{end}) for document of {len} bytes")]
    InvalidEditRange {
        /// Byte start offset of the edit.
        start: usize,
        /// Byte end offset of the edit.
        end: usize,
        /// Total `UTF-8` byte length of the source document.
        len: usize,
    },

    /// Two text edits in the same batch overlapped.
    #[error(
        "overlapping text edit ranges [{first_start}..{first_end}) and [{second_start}..{second_end})"
    )]
    OverlappingEdits {
        /// Byte start offset of the first edit.
        first_start: usize,
        /// Byte end offset of the first edit.
        first_end: usize,
        /// Byte start offset of the second edit.
        second_start: usize,
        /// Byte end offset of the second edit.
        second_end: usize,
    },
}
