//! VAL-UNIT-1: membership uses exact spelling and the declared unit system.

/// Refuse an unsupported system or absent code; never infer aliases or convert.
pub(crate) fn admitted<'a>(
    is_ucum: bool,
    code: &str,
    mut vocabulary: impl Iterator<Item = &'a str>,
) -> bool {
    is_ucum && vocabulary.any(|known| known == code)
}
