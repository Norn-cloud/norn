//! Norn (#3404): the legacy date value of upstream's `d()`/`date()` builtins and
//! date methods. None of them is registered (`FunctionRegistry`,
//! `MethodRegistry`), so no expression can build one: the type has no values.
//! The VM's date branches stay for upstream cherry-picks and are unreachable.
//! The pure temporal values live in `crate::temporal`.

use crate::variable::DynamicVariable;
use serde_json::Value;
use std::any::Any;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialOrd, PartialEq, Ord, Eq)]
pub(crate) enum VmDate {}

impl DynamicVariable for VmDate {
    fn type_name(&self) -> &'static str {
        "date"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn to_value(&self) -> Value {
        match *self {}
    }
}

impl Display for VmDate {
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        match *self {}
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
