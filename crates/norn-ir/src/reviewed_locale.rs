//! Shared admission of reviewed locale maps, including laws and question views.
use crate::deontic::valid_locale_tag;
use std::collections::{BTreeMap, BTreeSet};

/// REVIEWED-LOCALE-1: every entry is complete, tags are unique ignoring case,
/// and selection matches exactly without fallback. Preserve #496's wire errors.
///
/// # Errors
/// Returns the existing wording code for invalid, duplicate or incomplete entries.
pub fn validate<T>(
    entries: &BTreeMap<String, T>,
    validate_entry: fn(&T) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    if entries.is_empty() {
        return Err("law.wording.incomplete_locale");
    }
    let mut tags = BTreeSet::new();
    for (tag, entry) in entries {
        if !valid_locale_tag(tag) {
            return Err("law.wording.invalid_locale");
        }
        if !tags.insert(tag.to_ascii_lowercase()) {
            return Err("law.wording.duplicate_locale");
        }
        validate_entry(entry)?;
    }
    Ok(())
}

/// Select a well-formed exact locale tag, case insensitively, without fallback.
/// All entries are validated first; incomplete unselected translations refuse.
///
/// # Errors
/// Returns the existing wording code for an invalid or unavailable locale.
pub fn select<'a, T>(
    entries: &'a BTreeMap<String, T>,
    tag: &str,
    validate_entry: fn(&T) -> Result<(), &'static str>,
) -> Result<&'a T, &'static str> {
    validate(entries, validate_entry)?;
    if !valid_locale_tag(tag) {
        return Err("law.wording.invalid_locale");
    }
    entries
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(tag))
        .map(|(_, entry)| entry)
        .ok_or("law.wording.unknown_locale")
}

/// Decode a reviewed map without allowing duplicate wire keys to be overwritten.
///
/// # Errors
/// Refuses invalid/duplicate tags, missing or incomplete values and empty maps.
pub fn deserialize<'de, D, T>(
    deserializer: D,
    validate_entry: fn(&T) -> Result<(), &'static str>,
) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    struct LocaleVisitor<T> {
        validate_entry: fn(&T) -> Result<(), &'static str>,
    }
    impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for LocaleVisitor<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an ordered map of complete BCP 47 translations")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            use serde::de::Error as _;
            let mut entries = BTreeMap::new();
            let mut tags = BTreeSet::new();
            while let Some(tag) = map.next_key::<String>()? {
                if !valid_locale_tag(&tag) {
                    return Err(M::Error::custom("law.wording.invalid_locale"));
                }
                if !tags.insert(tag.to_ascii_lowercase()) {
                    return Err(M::Error::custom("law.wording.duplicate_locale"));
                }
                let entry = map.next_value::<T>().map_err(|error| {
                    M::Error::custom(format!("law.wording.incomplete_locale: {error}"))
                })?;
                entries.insert(tag, entry);
            }
            validate(&entries, self.validate_entry).map_err(M::Error::custom)?;
            Ok(entries)
        }
    }
    deserializer.deserialize_map(LocaleVisitor { validate_entry })
}

/// Escape bidirectional control characters in text-only display values.
#[must_use]
pub fn visible(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
        {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}
/// Isolate an identifier or expression after escaping embedded direction controls.
#[must_use]
pub fn isolate(text: &str) -> String {
    format!("\u{2068}{}\u{2069}", visible(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn complete<T: AsRef<str>>(value: &T) -> Result<(), &'static str> {
        if value.as_ref().trim().is_empty() {
            Err("law.wording.incomplete_locale")
        } else {
            Ok(())
        }
    }
    #[test]
    fn reviewed_locale_rejects_duplicate_wire_keys_and_incomplete_unselected_entries() {
        for source in [
            r#"{"en":"English","en":"Again"}"#,
            r#"{"en":"English","EN":"Again"}"#,
            r#"{"en":"English","ar":" "}"#,
        ] {
            assert!(
                deserialize(
                    &mut serde_json::Deserializer::from_str(source),
                    complete::<String>
                )
                .is_err()
            );
        }
        assert!(
            deserialize(
                &mut serde_json::Deserializer::from_str(r#"{"en":"English"}"#),
                complete::<String>
            )
            .is_ok()
        );
    }
    proptest::proptest! {
        #[test]
        fn reviewed_locale_selection_is_exact_without_fallback(requested in "[a-z]{2,8}") {
            let entries = BTreeMap::from([("en".to_owned(),"English".to_owned()),("ar".to_owned(),"العربية".to_owned())]);
            proptest::prop_assert!(validate(&entries, complete).is_ok());
            let selected = select(&entries, &requested, complete);
            proptest::prop_assert_eq!(selected.is_ok(), requested=="en" || requested=="ar");
            proptest::prop_assert_eq!(selected, select(&entries,&requested.to_ascii_uppercase(), complete));
        }
    }
}
