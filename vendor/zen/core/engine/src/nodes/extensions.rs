use crate::loader::{DynamicLoader, NoopLoader};
use std::sync::Arc;
use zen_expression::OpcodeCache;

/// This is created on every graph evaluation
#[derive(Debug, Clone)]
pub struct NodeHandlerExtensions {
    pub(crate) loader: DynamicLoader,
    pub(crate) compiled_cache: Option<Arc<OpcodeCache>>,
    /// Norn `metering`: shared by every node, isolate and sub-decision of one evaluation.
    pub(crate) meter: Option<zen_expression::meter::Meter>,
}

impl Default for NodeHandlerExtensions {
    fn default() -> Self {
        Self {
            loader: Arc::new(NoopLoader),
            compiled_cache: None,
            meter: None,
        }
    }
}

impl NodeHandlerExtensions {
    pub fn loader(&self) -> &DynamicLoader {
        &self.loader
    }
}
