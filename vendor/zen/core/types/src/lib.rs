#![forbid(unsafe_code)]
#![expect(
    clippy::type_complexity,
    reason = "Norn #3404: upstream layout kept so gorules/zen fixes still cherry-pick (vendor/zen)"
)]
#![expect(
    clippy::large_enum_variant,
    reason = "Norn #3404: upstream layout kept so gorules/zen fixes still cherry-pick (vendor/zen)"
)]

mod constant;
pub mod decision;
pub mod rccell;
pub mod rcvalue;
pub mod symbol;
pub mod variable;
pub mod variable_type;
