//! RFC 8259 number grammar, as an allocation-free scan over bytes.
//!
//! Kept free of `serde_json` and of iterator adapters, one function per
//! grammar part, so that the production code can be extracted and proved on
//! its own (#717). Every function takes and returns a cursor at most
//! `bytes.len()`.

/// Validate the RFC 8259 number grammar
/// `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`. Returns
/// `Some(true)` for an integer literal (no fraction, no exponent),
/// `Some(false)` for any other valid number, and `None` for invalid input.
pub(super) fn json_number_shape(bytes: &[u8]) -> Option<bool> {
    let cursor = after_sign(bytes, 0);
    let cursor = after_integer(bytes, cursor)?;
    let (cursor, fraction) = after_fraction(bytes, cursor)?;
    let (cursor, exponent) = after_exponent(bytes, cursor)?;
    if cursor == bytes.len() {
        Some(!fraction && !exponent)
    } else {
        None
    }
}

/// Past an optional `-`.
fn after_sign(bytes: &[u8], cursor: usize) -> usize {
    if cursor < bytes.len() && bytes[cursor] == b'-' {
        cursor + 1
    } else {
        cursor
    }
}

/// Past `0` or `[1-9][0-9]*`, or `None`.
fn after_integer(bytes: &[u8], cursor: usize) -> Option<usize> {
    if cursor >= bytes.len() {
        None
    } else if bytes[cursor] == b'0' {
        Some(cursor + 1)
    } else if bytes[cursor].is_ascii_digit() {
        Some(skip_digits(bytes, cursor + 1))
    } else {
        None
    }
}

/// Past an optional `. [0-9]+`, and whether there was one; `None` for a
/// point without digits.
fn after_fraction(bytes: &[u8], cursor: usize) -> Option<(usize, bool)> {
    if cursor < bytes.len() && bytes[cursor] == b'.' {
        let digits = skip_digits(bytes, cursor + 1);
        if digits == cursor + 1 {
            None
        } else {
            Some((digits, true))
        }
    } else {
        Some((cursor, false))
    }
}

/// Past an optional `[eE] [+-]? [0-9]+`, and whether there was one; `None`
/// for an exponent without digits.
fn after_exponent(bytes: &[u8], cursor: usize) -> Option<(usize, bool)> {
    if cursor < bytes.len() && (bytes[cursor] == b'e' || bytes[cursor] == b'E') {
        let start = after_exponent_sign(bytes, cursor + 1);
        let digits = skip_digits(bytes, start);
        if digits == start {
            None
        } else {
            Some((digits, true))
        }
    } else {
        Some((cursor, false))
    }
}

/// Past an optional `+` or `-`.
fn after_exponent_sign(bytes: &[u8], cursor: usize) -> usize {
    if cursor < bytes.len() && (bytes[cursor] == b'+' || bytes[cursor] == b'-') {
        cursor + 1
    } else {
        cursor
    }
}

/// The first index at or after `cursor` that is not an ASCII digit.
fn skip_digits(bytes: &[u8], mut cursor: usize) -> usize {
    while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
        cursor += 1;
    }
    cursor
}
