//! VAL-UNIT-1: membership uses exact spelling and the declared unit system.

/// Refuse an unsupported system or absent code; never infer aliases or convert.
pub(crate) fn admitted<'a>(
    is_ucum: bool,
    code: &str,
    mut vocabulary: impl Iterator<Item = &'a str>,
) -> bool {
    is_ucum && vocabulary.any(|known| known == code)
}

#[cfg(test)]
mod tests {
    use super::admitted;

    #[test]
    fn membership_respects_system_and_vocabulary_for_all_two_byte_codes() {
        for b0 in 0_u8..=u8::MAX {
            for b1 in 0_u8..=u8::MAX {
                let bytes = [b0, b1];
                let code = std::str::from_utf8(&bytes).unwrap_or("");
                for is_ucum in [false, true] {
                    assert_eq!(
                        admitted(is_ucum, code, ["mg", "kg"].iter().copied()),
                        is_ucum && matches!(bytes, [b'm', b'g'] | [b'k', b'g'])
                    );
                    assert!(!admitted(is_ucum, code, std::iter::empty()));
                }
            }
        }
    }
}
