//! Norn (#3404, #1436): the legacy date value of upstream's `d()`/`date()`
//! builtins and date methods. None of them is registered (`FunctionRegistry`,
//! `MethodRegistry`), so nothing constructs one: the field is private and no
//! constructor exists. The VM's date branches stay for upstream cherry-picks
//! and never match.
//! The pure temporal values live in `crate::temporal`.

use crate::variable::DynamicVariable;
use serde_json::Value;
use std::any::Any;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialOrd, PartialEq, Ord, Eq)]
#[allow(dead_code, reason = "never constructed; see the module documentation")]
pub(crate) struct VmDate(());

impl DynamicVariable for VmDate {
    fn type_name(&self) -> &'static str {
        "date"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn to_value(&self) -> Value {
        unreachable!("no legacy date value is ever constructed")
    }
}

impl Display for VmDate {
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        unreachable!("no legacy date value is ever constructed")
    }
}

pub(crate) trait DynamicVariableExt {
    fn as_date(&self) -> Option<&VmDate>;
}

impl DynamicVariableExt for dyn DynamicVariable {
    fn as_date(&self) -> Option<&VmDate> {
        self.as_any().downcast_ref::<VmDate>()
    }
}
