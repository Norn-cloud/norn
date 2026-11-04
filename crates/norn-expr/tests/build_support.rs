#![allow(missing_docs, reason = "integration-test crate")]

include!("../build_support.rs");

#[test]
fn lf_and_crlf_produce_identical_fingerprint_input() {
    let lf = "fn example() {\n    // café\n}\n";
    assert_eq!(normalize_source(lf), lf);
    assert_eq!(normalize_source(&lf.replace('\n', "\r\n")), lf);
}

#[test]
fn lone_cr_is_preserved() {
    assert_eq!(normalize_source("a\r\nb\rc\n"), "a\nb\rc\n");
}

#[test]
fn lone_cr_in_comment_keeps_semantically_distinct_sources_distinct() {
    let lone_cr = "fn f()->u8 { // comment\rreturn 1;\n2 }";
    let lf = "fn f()->u8 { // comment\nreturn 1;\n2 }";
    assert_eq!(normalize_source(lone_cr), lone_cr);
    assert_ne!(normalize_source(lone_cr), normalize_source(lf));
}
