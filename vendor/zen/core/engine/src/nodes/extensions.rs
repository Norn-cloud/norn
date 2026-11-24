use crate::loader::{DynamicLoader, NoopLoader};
use std::sync::Arc;
use zen_expression::OpcodeCache;

/// This is created on every graph evaluation
#[derive(Debug, Clone)]
pub struct NodeHandlerExtensions {
    pub(crate) loader: DynamicLoader,
    pub(crate) contracts: Option<Arc<dyn DecisionContracts>>,
    pub(crate) compiled_cache: Option<Arc<OpcodeCache>>,
    /// Norn `metering`: shared by every node, isolate and sub-decision of one evaluation.
    pub(crate) meter: Option<zen_expression::meter::Meter>,
    /// Norn (#1440): explicit versioned offline IANA tzdb lookup shared across nodes.
    pub(crate) tzdb: Option<Arc<dyn zen_expression::temporal::TzdbLookup>>,
}

impl Default for NodeHandlerExtensions {
    fn default() -> Self {
        Self {
            loader: Arc::new(NoopLoader),
            contracts: None,
            compiled_cache: None,
            meter: None,
            tzdb: None,
        }
    }
}

impl NodeHandlerExtensions {
    pub fn loader(&self) -> &DynamicLoader {
        &self.loader
    }
}

/// Exact host-owned schemas at every sub-decision boundary.
/// One host-checked binding; execution remains in the shared expression VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpressionContract {
    pub source: String,
    pub exact: bool,
}

pub trait DecisionContracts: std::fmt::Debug + Send + Sync {
    fn expression(&self, _node: &str, _key: &str, _source: &str) -> Option<ExpressionContract> {
        None
    }

    fn check(
        &self,
        key: &str,
        value: &zen_expression::Variable,
        input: bool,
        meter: Option<&zen_expression::meter::Meter>,
    ) -> Result<(), Box<dyn std::error::Error>>;
    fn derive(
        &self,
        _key: &str,
        _input: &zen_expression::Variable,
        _meter: Option<&zen_expression::meter::Meter>,
    ) -> Option<Result<zen_expression::Variable, Box<dyn std::error::Error>>> {
        None
    }
    fn derive_with_tzdb(
        &self,
        key: &str,
        input: &zen_expression::Variable,
        meter: Option<&zen_expression::meter::Meter>,
        _tzdb: Option<&Arc<dyn zen_expression::temporal::TzdbLookup>>,
    ) -> Option<Result<zen_expression::Variable, Box<dyn std::error::Error>>> {
        self.derive(key, input, meter)
    }
}
