//! Exhaustive correspondence cases for the closed `RuleIR` hit-policy admission predicate.

use norn_ir::rule_hit_policy_admits_match;

#[test]
fn exact_hit_policy_admission_truth_table_is_closed() {
    for first_hit_policy in [false, true] {
        for has_match in [false, true] {
            for has_multiple_matches in [false, true] {
                let admitted =
                    rule_hit_policy_admits_match(first_hit_policy, has_match, has_multiple_matches);
                let expected = has_match && (first_hit_policy || !has_multiple_matches);
                assert_eq!(admitted, expected);
            }
        }
    }
}

#[test]
fn unique_never_admits_overlap_and_first_requires_a_match() {
    assert!(!rule_hit_policy_admits_match(false, true, true));
    assert!(rule_hit_policy_admits_match(false, true, false));
    assert!(rule_hit_policy_admits_match(true, true, true));
    assert!(!rule_hit_policy_admits_match(true, false, false));
}
