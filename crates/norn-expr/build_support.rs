// Match Rust input normalization: CRLF becomes LF; a lone CR is significant.
fn normalize_source(contents: &str) -> String {
    contents.replace("\r\n", "\n")
}
