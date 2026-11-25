//! PRIVACY-GUARD-1's text passes: where a marked value occurs in a prompt,
//! and the detector for email addresses, phone numbers and payment cards.

/// Every occurrence of `needle` in `haystack` (ASCII case-insensitive), as
/// byte ranges.
pub(super) fn occurrences(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    let (h, n) = (haystack.to_lowercase(), needle.to_lowercase());
    if n.is_empty() || h.len() != haystack.len() || n.len() != needle.len() {
        // Lower-casing changed byte lengths: match exactly instead.
        return haystack
            .match_indices(needle)
            .map(|(at, _)| (at, at.saturating_add(needle.len())))
            .collect();
    }
    h.match_indices(&n)
        .map(|(at, _)| (at, at.saturating_add(n.len())))
        .collect()
}

/// A detector match: kind and byte range.
pub(super) fn detect(text: &str) -> Vec<(&'static str, usize, usize)> {
    let mut found = emails(text);
    found.extend(digit_runs(text));
    found
}

/// Email addresses: `local@domain.tld` around each `@`.
fn emails(text: &str) -> Vec<(&'static str, usize, usize)> {
    let bytes = text.as_bytes();
    let local = |b: u8| b.is_ascii_alphanumeric() || b"._%+-".contains(&b);
    let domain = |b: u8| b.is_ascii_alphanumeric() || b".-".contains(&b);
    let mut found = Vec::new();
    for (at, _) in text.match_indices('@') {
        let mut start = at;
        while let Some(previous) = start.checked_sub(1)
            && bytes.get(previous).copied().is_some_and(local)
        {
            start = previous;
        }
        let host_start = at.saturating_add(1);
        let mut end = host_start;
        while bytes.get(end).copied().is_some_and(domain) {
            end = end.saturating_add(1);
        }
        while end > host_start && end.checked_sub(1).and_then(|last| bytes.get(last)) == Some(&b'.')
        {
            end = end.saturating_sub(1);
        }
        let host = text.get(host_start..end).unwrap_or("");
        if start < at && host.contains('.') && !host.starts_with('.') {
            found.push(("EMAIL", start, end));
        }
    }
    found
}

/// Digit runs with separators: payment cards (13 to 19 digits, Luhn-valid)
/// and phone numbers (8 to 15 digits, written with `+` or separators).
fn digit_runs(text: &str) -> Vec<(&'static str, usize, usize)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut index = 0_usize;
    while index < bytes.len() {
        let preceded = index
            .checked_sub(1)
            .and_then(|previous| bytes.get(previous))
            .is_some_and(u8::is_ascii_alphanumeric);
        let starts = bytes
            .get(index)
            .is_some_and(|b| b.is_ascii_digit() || *b == b'+')
            && !preceded;
        if !starts {
            index = index.saturating_add(1);
            continue;
        }
        let mut end = index;
        let mut digits = String::new();
        while let Some(&b) = bytes.get(end) {
            if b.is_ascii_digit() {
                digits.push(char::from(b));
            } else if !(b" -().".contains(&b) || (b == b'+' && end == index)) {
                break;
            }
            end = end.saturating_add(1);
        }
        // Do not end on a separator.
        while end > index
            && !end
                .checked_sub(1)
                .and_then(|last| bytes.get(last))
                .is_some_and(u8::is_ascii_digit)
        {
            end = end.saturating_sub(1);
        }
        let card = (13..=19).contains(&digits.len()) && luhn(&digits);
        let phone = !card
            && (8..=15).contains(&digits.len())
            && (bytes.get(index) == Some(&b'+')
                || text
                    .get(index..end)
                    .is_some_and(|run| run.contains([' ', '-', '('])));
        if card {
            found.push(("CARD", index, end));
        } else if phone {
            found.push(("PHONE", index, end));
        }
        index = end.max(index.saturating_add(1));
    }
    found
}

fn luhn(digits: &str) -> bool {
    let mut sum = 0_u32;
    for (position, c) in digits.chars().rev().enumerate() {
        let d = c.to_digit(10).unwrap_or(0);
        let weighted = if position % 2 == 1 {
            let doubled = d.saturating_mul(2);
            if doubled > 9 {
                doubled.saturating_sub(9)
            } else {
                doubled
            }
        } else {
            d
        };
        sum = sum.saturating_add(weighted);
    }
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::{detect, luhn};

    #[test]
    fn the_detector_finds_emails_phones_and_luhn_valid_cards() {
        let text = "Reach sofia.r@fastmail.com or +1 415 555 0132; card 4539 1488 0343 6467, order 20241024.";
        let kinds: Vec<&str> = detect(text).into_iter().map(|(kind, _, _)| kind).collect();
        assert_eq!(kinds, ["EMAIL", "PHONE", "CARD"]);
        assert!(luhn("4539148803436467"));
        assert!(!luhn("4539148803436468"));
    }
}
