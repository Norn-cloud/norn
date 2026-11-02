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
