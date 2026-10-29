#![expect(
    clippy::disallowed_types,
    reason = "Norn #3404: these maps use BuildNoHashHasher (identity hashing), so iteration order is deterministic"
)]
use crate::functions::defs::FunctionDefinition;
use crate::functions::{DeprecatedFunction, FunctionKind, InternalFunction};
use nohash_hasher::{BuildNoHashHasher, IsEnabled};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use strum::IntoEnumIterator;

impl IsEnabled for InternalFunction {}
impl IsEnabled for DeprecatedFunction {}

pub struct FunctionRegistry {
    internal_functions:
        HashMap<InternalFunction, Rc<dyn FunctionDefinition>, BuildNoHashHasher<InternalFunction>>,
    deprecated_functions: HashMap<
        DeprecatedFunction,
        Rc<dyn FunctionDefinition>,
        BuildNoHashHasher<DeprecatedFunction>,
    >,
}

impl FunctionRegistry {
    thread_local!(
        static INSTANCE: RefCell<FunctionRegistry> = RefCell::new(FunctionRegistry::new_internal())
    );

    pub fn get_definition(kind: &FunctionKind) -> Option<Rc<dyn FunctionDefinition>> {
        match kind {
            FunctionKind::Internal(internal) => {
                Self::INSTANCE.with_borrow(|i| i.internal_functions.get(internal).cloned())
            }
            FunctionKind::Deprecated(deprecated) => {
                Self::INSTANCE.with_borrow(|i| i.deprecated_functions.get(deprecated).cloned())
            }
            FunctionKind::Closure(_) => None,
        }
    }

    fn new_internal() -> Self {
        let internal_functions = InternalFunction::iter()
            .filter(|i| *i != InternalFunction::Date)
            .map(|i| (i, (&i).into()))
            .collect();

        // Norn (#3404): the legacy deprecated functions (ambient clock, timezone) are
        // not registered.
        let deprecated_functions = HashMap::default();

        Self {
            internal_functions,
            deprecated_functions,
        }
    }
}
