//! RFC 8259 number grammar, as an allocation-free scan over bytes.
//!
//! Kept free of `serde_json` and of iterator adapters, one function per
//! grammar part, so that the production code can be extracted and proved on
//! its own (#717). Every function takes and returns a cursor at most
//! `bytes.len()`; the `checked_add` refusals are unreachable for slices.

/// Validate the RFC 8259 number grammar
/// `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`. Returns
/// `Some(true)` for an integer literal (no fraction, no exponent),
/// `Some(false)` for any other valid number, and `None` for invalid input.
pub(super) fn json_number_shape(bytes: &[u8]) -> Option<bool> {
    let cursor = after_sign(bytes, 0)?;
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
fn after_sign(bytes: &[u8], cursor: usize) -> Option<usize> {
    if let Some(b'-') = bytes.get(cursor) {
        cursor.checked_add(1)
    } else {
        Some(cursor)
    }
}

/// Past `0` or `[1-9][0-9]*`, or `None`.
fn after_integer(bytes: &[u8], cursor: usize) -> Option<usize> {
    match bytes.get(cursor) {
        Some(b'0') => cursor.checked_add(1),
        Some(byte) => {
            if byte.is_ascii_digit() {
                skip_digits(bytes, cursor.checked_add(1)?)
            } else {
                None
            }
        }
        None => None,
    }
}

/// Past an optional `. [0-9]+`, and whether there was one; `None` for a
/// point without digits.
fn after_fraction(bytes: &[u8], cursor: usize) -> Option<(usize, bool)> {
    if let Some(b'.') = bytes.get(cursor) {
        let start = cursor.checked_add(1)?;
        let digits = skip_digits(bytes, start)?;
        if digits == start {
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
    if let Some(b'e' | b'E') = bytes.get(cursor) {
        let start = after_exponent_sign(bytes, cursor.checked_add(1)?)?;
        let digits = skip_digits(bytes, start)?;
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
fn after_exponent_sign(bytes: &[u8], cursor: usize) -> Option<usize> {
    if let Some(b'+' | b'-') = bytes.get(cursor) {
        cursor.checked_add(1)
    } else {
        Some(cursor)
    }
}

/// The first index at or after `cursor` that is not an ASCII digit.
fn skip_digits(bytes: &[u8], mut cursor: usize) -> Option<usize> {
    loop {
        match bytes.get(cursor) {
            Some(byte) => {
                if byte.is_ascii_digit() {
                    match cursor.checked_add(1) {
                        Some(next) => cursor = next,
                        None => break None,
                    }
                } else {
                    break Some(cursor);
                }
            }
            None => break Some(cursor),
        }
    }
}
