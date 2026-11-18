//! Norn (#3404): the names of upstream's deprecated date builtins. The parser
//! still recognises them so a call is reported, but none is registered
//! (`FunctionRegistry`) and their chrono/humantime implementations were removed.

use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

#[derive(Debug, PartialEq, Eq, Hash, Display, EnumString, EnumIter, IntoStaticStr, Clone, Copy)]
#[strum(serialize_all = "camelCase")]
pub enum DeprecatedFunction {
    Date,
    Time,
    Duration,
    Year,
    DayOfWeek,
    DayOfMonth,
    DayOfYear,
    WeekOfYear,
    MonthOfYear,
    MonthString,
    DateString,
    WeekdayString,
    StartOf,
    EndOf,
}
