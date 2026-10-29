#![expect(
    clippy::disallowed_types,
    reason = "Norn #3404: these maps use BuildNoHashHasher (identity hashing), so iteration order is deterministic"
)]
use crate::functions::date_method::DateMethod;
use crate::functions::defs::FunctionDefinition;
use nohash_hasher::{BuildNoHashHasher, IsEnabled};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Display;
use std::rc::Rc;

impl IsEnabled for DateMethod {}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum MethodKind {
    DateMethod(DateMethod),
}

impl TryFrom<&str> for MethodKind {
    type Error = strum::ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        DateMethod::try_from(value).map(MethodKind::DateMethod)
    }
}

impl Display for MethodKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MethodKind::DateMethod(d) => write!(f, "{d}"),
        }
    }
}

pub struct MethodRegistry {
    date_methods: HashMap<DateMethod, Rc<dyn FunctionDefinition>, BuildNoHashHasher<DateMethod>>,
}

impl MethodRegistry {
    thread_local!(
        static INSTANCE: RefCell<MethodRegistry> = RefCell::new(MethodRegistry::new_internal())
    );

    pub fn get_definition(kind: &MethodKind) -> Option<Rc<dyn FunctionDefinition>> {
        match kind {
            MethodKind::DateMethod(dm) => {
                Self::INSTANCE.with_borrow(|i| i.date_methods.get(dm).cloned())
            }
        }
    }

    fn new_internal() -> Self {
        // Norn (#3404): legacy date methods are not registered.
        let date_methods = HashMap::default();

        Self { date_methods }
    }
}
