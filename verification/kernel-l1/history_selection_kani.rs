//! ASOF-KNOWLEDGE-1: bounded selection ordering and old-answer preservation.
#[path = "../../crates/norn-cell/src/history_selection.rs"]
mod selection;
use selection::select_known_version;
#[kani::proof]
fn selection_commutes_and_future_knowledge_preserves_old_answers() {
    let cutoff: u64 = kani::any();
    let a: u64 = kani::any(); let a_at: u64 = kani::any();
    let b: u64 = kani::any(); let b_at: u64 = kani::any();
    let old=select_known_version(cutoff,None,a,a_at);
    let forward=select_known_version(cutoff,old,b,b_at);
    let reverse=select_known_version(cutoff,select_known_version(cutoff,None,b,b_at),a,a_at);
    assert_eq!(forward,reverse);
    if a_at>cutoff { assert_eq!(old,None); } else { assert_eq!(old,Some(a)); }
    if b_at>cutoff { assert_eq!(forward,old); }
}

#[kani::proof]
fn receipt_prefix_including_empty_commits_matches_wide_arithmetic() {
    let first:u64=kani::any(); let count:u64=kani::any();
    let expected=u128::from(first).checked_sub(1).and_then(|n|n.checked_add(u128::from(count))).and_then(|n|<u64 as core::convert::TryFrom<u128>>::try_from(n).ok());
    assert_eq!(selection::receipt_last_sequence(first,count),expected);
}

#[kani::proof]
fn history_steps_never_rewind_or_skip_receipts() {
    let a:u64=kani::any();let ka:u64=kani::any();let b:u64=kani::any();let kb:u64=kani::any();
    assert_eq!(selection::next_known_slice(a,ka,b,kb),b>=a && u128::from(b)<=u128::from(a)+1 && kb>=ka);
}
