//! Norn (#3404): the names of upstream's legacy date methods. The parser still
//! recognises them so a call is reported, but none is registered
//! (`MethodRegistry`) and their chrono-based implementations were removed.

use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

#[derive(Debug, PartialEq, Eq, Hash, Display, EnumString, EnumIter, IntoStaticStr, Clone, Copy)]
#[strum(serialize_all = "camelCase")]
pub enum DateMethod {
    Add,
    Sub,
    Set,
    Format,
    StartOf,
    EndOf,
    Diff,
    Tz,

    // Compare
    IsSame,
    IsBefore,
    IsAfter,
    IsSameOrBefore,
    IsSameOrAfter,

    // Getters
    Second,
    Minute,
    Hour,
    Day,
    DayOfYear,
    Week,
    Weekday,
    Month,
    Quarter,
    Year,
    Timestamp,
    OffsetName,

    IsValid,
    IsYesterday,
    IsToday,
    IsTomorrow,
    IsLeapYear,
}
