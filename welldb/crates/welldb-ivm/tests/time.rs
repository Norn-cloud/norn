//! Synthetic boundary tokens, checked against independent calendar/epoch identities.
use welldb_ivm::semantics::parse_time;
#[test]
fn epochs_offsets_and_nanoseconds_are_exact() {
    assert_eq!(parse_time("1970-01-01"), Some(0));
    assert_eq!(parse_time("1969-12-31"), Some(-86_400_000_000_000));
    assert_eq!(parse_time("2000-02-29"), Some(951_782_400_000_000_000));
    for text in [
        "1970-01-01T00:00:00Z",
        "1970-01-01T01:00:00+01:00",
        "1969-12-31T23:00:00-01:00",
    ] {
        assert_eq!(parse_time(text), Some(0));
    }
    assert_eq!(parse_time("1970-01-01T00:00:00.000000001Z"), Some(1));
    assert_eq!(parse_time("1970-01-01T00:00:00.1Z"), Some(100_000_000));
}
#[test]
fn four_digit_calendar_range_exceeds_native_i64_nanos_without_wrapping() {
    assert_eq!(parse_time("0000-01-01"), Some(-62_167_219_200_000_000_000));
    assert_eq!(
        parse_time("9999-12-31T23:59:59.999999999Z"),
        Some(253_402_300_799_999_999_999)
    );
    assert_eq!(parse_time("0000-01-01T00:00:00+00:01"), None);
    assert_eq!(parse_time("9999-12-31T23:59:59-00:01"), None);
}
#[test]
fn invalid_calendar_precision_and_unknown_offsets_refuse() {
    for text in [
        "1900-02-29",
        "2100-02-29",
        "2000-02-30",
        "1970-13-01",
        "1970-01-00",
        "1970-01-01T24:00:00Z",
        "1970-01-01T00:60:00Z",
        "1970-01-01T00:00:60Z",
        "1970-01-01T00:00:00-00:00",
        "1970-01-01T00:00:00+24:00",
        "1970-01-01T00:00:00+00:60",
        "1970-01-01T00:00:00.1234567890Z",
        "1970-01-01T00:00:00.Z",
        "1970-01-01T00:00:00",
        "10000-01-01",
        "1970-01-01junk",
        "é970-01-01",
    ] {
        assert_eq!(parse_time(text), None, "{text}");
    }
}
