//! PARAM-EXACT-1: bounded proof of the production half-open interval selector.
#[path = "../../crates/norn-checker/src/parameter_window.rs"]
mod selection;
use selection::{ParameterError, ParameterWindow, unique_active};

#[kani::proof]
#[kani::unwind(5)]
fn unique_half_open_recorded_winner() {
    let rows: [ParameterWindow; 3] = std::array::from_fn(|_| ParameterWindow {
        from: kani::any(),
        to: kani::any(),
        revision: kani::any(),
    });
    let at = kani::any();
    let revision = kani::any();
    let result = unique_active(&rows, at, revision);
    if rows.iter().any(|r| r.to.is_some_and(|end| end <= r.from)) {
        assert_eq!(result, Err(ParameterError::Invalid));
    } else {
        let active: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.revision <= revision && r.from <= at && r.to.is_none_or(|end| at < end)
            })
            .map(|(i, _)| i)
            .collect();
        match active.as_slice() {
            [] => assert_eq!(result, Ok(None)),
            [one] => assert_eq!(result, Ok(Some(*one))),
            _ => assert_eq!(result, Err(ParameterError::Ambiguous)),
        }
    }
}
