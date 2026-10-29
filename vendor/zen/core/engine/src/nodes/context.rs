use crate::nodes::definition::{NodeDataType, TraceDataType};
use crate::nodes::extensions::NodeHandlerExtensions;
use crate::nodes::result::{NodeResponse, NodeResult};
use crate::nodes::NodeError;
use crate::ZEN_CONFIG;
use serde_json::Value;
use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use zen_expression::Isolate;
use zen_types::variable::{ToVariable, Variable};

#[derive(Clone)]
pub struct NodeContext<NodeData, TraceData>
where
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub node: NodeData,
    pub input: Variable,
    pub nodes: Option<Variable>,
    pub trace: Option<RefCell<TraceData>>,
    pub extensions: NodeHandlerExtensions,
    pub iteration: u8,
    pub config: NodeContextConfig,
}

impl<NodeData, TraceData> NodeContext<NodeData, TraceData>
where
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    pub fn input_with_nodes(&self) -> Variable {
        let Some(nodes) = &self.nodes else {
            return self.input.shallow_clone();
        };
        let Variable::Object(object) = &self.input else {
            return self.input.shallow_clone();
        };

        let mut map = object.borrow().clone();
        map.insert(Variable::nodes_key(), nodes.clone());
        Variable::from_object(map)
    }

    pub fn from_base(base: NodeContextBase, data: NodeData) -> Self {
        Self {
            id: base.id,
            name: base.name,
            input: base.input,
            nodes: base.nodes,
            extensions: base.extensions,
            iteration: base.iteration,
            trace: base.config.trace.then(Default::default),
            node: data,
            config: base.config,
        }
    }

    pub fn isolate(&self) -> Isolate {
        make_isolate(&self.input, self.nodes.as_ref(), &self.extensions)
    }

    pub fn trace<Function>(&self, mutator: Function)
    where
        Function: FnOnce(&mut TraceData),
    {
        if let Some(trace) = &self.trace {
            mutator(&mut *trace.borrow_mut());
        }
    }

    pub fn error<Error>(&self, error: Error) -> NodeResult
    where
        Error: Into<Box<dyn std::error::Error>>,
    {
        Err(self.make_error(error))
    }

    pub fn success(&self, output: Variable) -> NodeResult {
        Ok(NodeResponse {
            output,
            trace_data: self.trace.as_ref().map(|v| (*v.borrow()).to_variable()),
        })
    }

    pub(crate) fn make_error<Error>(&self, error: Error) -> NodeError
    where
        Error: Into<Box<dyn std::error::Error>>,
    {
        NodeError {
            node_id: self.id.clone(),
            trace: self.trace.as_ref().map(|v| (*v.borrow()).to_variable()),
            source: error.into(),
        }
    }

    /// Norn (C8): node-level JSON Schema validation is not part of the engine; a node
    /// schema is never silently skipped, it fails closed with a typed
    /// [`crate::SchemaCompileError`].
    pub fn validate(&self, _schema: &Value, _value: &Variable) -> Result<(), NodeError> {
        Err(self.make_error(crate::SchemaCompileError {
            message: "JSON Schema validation of node schemas is not supported by this engine"
                .to_owned(),
            unresolved_reference: false,
        }))
    }
}

pub trait NodeContextExt<T, Context>: Sized {
    type Error: Into<Box<dyn std::error::Error>>;

    fn with_node_context<Function, NewError>(
        self,
        ctx: &Context,
        f: Function,
    ) -> Result<T, NodeError>
    where
        Function: FnOnce(Self::Error) -> NewError,
        NewError: Into<Box<dyn std::error::Error>>;

    fn node_context(self, ctx: &Context) -> Result<T, NodeError> {
        self.with_node_context(ctx, |e| e.into())
    }

    fn node_context_message(self, ctx: &Context, message: &str) -> Result<T, NodeError> {
        self.with_node_context(ctx, |err| format!("{}: {}", message, err.into()))
    }
}

impl<T, E, NodeData, TraceData> NodeContextExt<T, NodeContext<NodeData, TraceData>> for Result<T, E>
where
    E: Into<Box<dyn std::error::Error>>,
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    type Error = E;

    fn with_node_context<Function, NewError>(
        self,
        ctx: &NodeContext<NodeData, TraceData>,
        f: Function,
    ) -> Result<T, NodeError>
    where
        Function: FnOnce(Self::Error) -> NewError,
        NewError: Into<Box<dyn std::error::Error>>,
    {
        self.map_err(|err| ctx.make_error(f(err)))
    }
}

impl<T, NodeData, TraceData> NodeContextExt<T, NodeContext<NodeData, TraceData>> for Option<T>
where
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    type Error = &'static str;

    fn with_node_context<Function, NewError>(
        self,
        ctx: &NodeContext<NodeData, TraceData>,
        f: Function,
    ) -> Result<T, NodeError>
    where
        Function: FnOnce(Self::Error) -> NewError,
        NewError: Into<Box<dyn std::error::Error>>,
    {
        self.ok_or_else(|| ctx.make_error(f("None")))
    }

    fn node_context_message(
        self,
        ctx: &NodeContext<NodeData, TraceData>,
        message: &str,
    ) -> Result<T, NodeError> {
        self.with_node_context(ctx, |_| message.to_string())
    }
}

#[derive(Clone)]
pub struct NodeContextBase {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub input: Variable,
    pub nodes: Option<Variable>,
    pub iteration: u8,
    pub extensions: NodeHandlerExtensions,
    pub config: NodeContextConfig,
    pub trace: Option<RefCell<Variable>>,
}

pub(crate) fn make_isolate(
    input: &Variable,
    nodes: Option<&Variable>,
    extensions: &NodeHandlerExtensions,
) -> Isolate {
    let mut isolate =
        Isolate::with_environment(input.clone()).with_cache(extensions.compiled_cache.clone());
    if let Some(nodes) = nodes {
        isolate.set_local(Variable::nodes_key(), nodes.clone());
    }
    isolate.set_meter(extensions.meter.clone());

    isolate
}

impl NodeContextBase {
    pub fn isolate(&self) -> Isolate {
        make_isolate(&self.input, self.nodes.as_ref(), &self.extensions)
    }

    pub fn error<Error>(&self, error: Error) -> NodeResult
    where
        Error: Into<Box<dyn std::error::Error>>,
    {
        Err(self.make_error(error))
    }

    pub fn success(&self, output: Variable) -> NodeResult {
        Ok(NodeResponse {
            output,
            trace_data: self.trace.as_ref().map(|v| v.borrow().to_variable()),
        })
    }

    fn make_error<Error>(&self, error: Error) -> NodeError
    where
        Error: Into<Box<dyn std::error::Error>>,
    {
        NodeError {
            node_id: self.id.clone(),
            trace: self.trace.as_ref().map(|t| t.borrow().to_variable()),
            source: error.into(),
        }
    }

    pub fn trace<Function>(&self, mutator: Function)
    where
        Function: FnOnce(&mut Variable),
    {
        if let Some(trace) = &self.trace {
            mutator(&mut trace.borrow_mut());
        }
    }
}

impl<NodeData, TraceData> From<NodeContext<NodeData, TraceData>> for NodeContextBase
where
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    fn from(value: NodeContext<NodeData, TraceData>) -> Self {
        let trace = match value.config.trace {
            true => Some(RefCell::new(Variable::Null)),
            false => None,
        };

        Self {
            id: value.id,
            name: value.name,
            input: value.input,
            nodes: value.nodes,
            extensions: value.extensions,
            iteration: value.iteration,
            config: value.config,
            trace,
        }
    }
}

impl<T, E> NodeContextExt<T, NodeContextBase> for Result<T, E>
where
    E: Into<Box<dyn std::error::Error>>,
{
    type Error = E;

    fn with_node_context<Function, NewError>(
        self,
        ctx: &NodeContextBase,
        f: Function,
    ) -> Result<T, NodeError>
    where
        Function: FnOnce(Self::Error) -> NewError,
        NewError: Into<Box<dyn std::error::Error>>,
    {
        self.map_err(|err| ctx.make_error(f(err)))
    }
}

impl<T> NodeContextExt<T, NodeContextBase> for Option<T> {
    type Error = &'static str;

    fn with_node_context<Function, NewError>(
        self,
        ctx: &NodeContextBase,
        f: Function,
    ) -> Result<T, NodeError>
    where
        Function: FnOnce(Self::Error) -> NewError,
        NewError: Into<Box<dyn std::error::Error>>,
    {
        self.ok_or_else(|| ctx.make_error(f("None")))
    }

    fn node_context_message(self, ctx: &NodeContextBase, message: &str) -> Result<T, NodeError> {
        self.with_node_context(ctx, |_| message.to_string())
    }
}

#[derive(Clone)]
pub struct NodeContextConfig {
    pub trace: bool,
    pub nodes_in_context: bool,
    pub max_depth: u8,
}

impl Default for NodeContextConfig {
    fn default() -> Self {
        Self {
            trace: false,
            nodes_in_context: ZEN_CONFIG.nodes_in_context.load(Ordering::Relaxed),
            max_depth: 5,
        }
    }
}
