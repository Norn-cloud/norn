use vstd::prelude::*;

verus! {
// The executable body is compared with production before verification.
pub fn checked_next_version(version: u64) -> (result: Option<u64>)
    ensures
        result.is_none() <==> version == u64::MAX,
        match result {
            Some(next) => next as int == version as int + 1,
            None => true,
        },
{
    version.checked_add(1)
}
}
fn main() {}
