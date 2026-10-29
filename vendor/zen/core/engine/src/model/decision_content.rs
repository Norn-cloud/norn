use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;
use zen_expression::{ExpressionKind, Isolate, OpcodeCache};
use zen_types::decision::{DecisionEdge, DecisionNode, DecisionNodeKind};

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum DecisionContent {
    Graph(Arc<GraphContent>),
}

impl<'de> Deserialize<'de> for DecisionContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        serde_path_to_error::deserialize::<_, GraphContent>(value)
            .map(|graph| Self::Graph(Arc::new(graph)))
            .map_err(serde::de::Error::custom)
    }
}

impl Default for DecisionContent {
    fn default() -> Self {
        Self::Graph(Arc::new(GraphContent::default()))
    }
}

impl DecisionContent {
    pub fn as_graph(&self) -> Option<&GraphContent> {
        match self {
            Self::Graph(g) => Some(g),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Graph(_) => "graph",
        }
    }

    pub fn into_graph_arc(self: Arc<Self>) -> Option<Arc<GraphContent>> {
        match self.as_ref() {
            Self::Graph(g) => Some(g.clone()),
        }
    }
}

impl From<GraphContent> for DecisionContent {
    fn from(value: GraphContent) -> Self {
        Self::Graph(Arc::new(value))
    }
}

impl From<Arc<GraphContent>> for DecisionContent {
    fn from(value: Arc<GraphContent>) -> Self {
        Self::Graph(value)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GraphContent {
    pub nodes: Vec<Arc<DecisionNode>>,
    pub edges: Vec<Arc<DecisionEdge>>,

    #[serde(skip)]
    pub compiled_cache: Option<Arc<OpcodeCache>>,
}

impl GraphContent {
    pub fn compile(&mut self) {
        if self.compiled_cache.is_some() {
            return;
        }

        let mut sources: Vec<(Arc<str>, ExpressionKind)> = Vec::new();
        for node in &self.nodes {
            match &node.kind {
                DecisionNodeKind::ExpressionNode { content } => {
                    for expr in content.expressions.iter() {
                        if !expr.key.is_empty() && !expr.value.is_empty() {
                            sources.push((expr.value.clone(), ExpressionKind::Standard));
                        }
                    }
                }
                DecisionNodeKind::DecisionTableNode { content } => {
                    for rule in content.rules.iter() {
                        for input in content.inputs.iter() {
                            let Some(rule_value) = rule.get(&input.id) else {
                                continue;
                            };

                            let kind = if input.field.is_some() {
                                ExpressionKind::Unary
                            } else {
                                ExpressionKind::Standard
                            };

                            sources.push((rule_value.clone(), kind));
                        }

                        for output in content.outputs.iter() {
                            let Some(rule_value) = rule.get(&output.id) else {
                                continue;
                            };

                            sources.push((rule_value.clone(), ExpressionKind::Standard));
                        }
                    }
                }
                _ => {}
            }
        }

        let mut cache: OpcodeCache = OpcodeCache::new();
        let mut isolate = Isolate::new();

        for (source, kind) in &sources {
            let map = match kind {
                ExpressionKind::Standard => &mut cache.standard,
                ExpressionKind::Unary => &mut cache.unary,
            };
            if map.contains_key(source) {
                continue;
            }

            let result = match kind {
                ExpressionKind::Standard => isolate
                    .compile_standard(source)
                    .map(|e| e.bytecode().to_vec()),
                ExpressionKind::Unary => {
                    isolate.compile_unary(source).map(|e| e.bytecode().to_vec())
                }
            };
            if let Ok(bytecode) = result {
                map.insert(source.clone(), Arc::from(bytecode));
            }
        }

        self.compiled_cache.replace(Arc::new(cache));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_graph_error_mentions_field() {
        let error = serde_json::from_str::<DecisionContent>(r#"{"nodes":[],"edges":"bad"}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("edges"), "{error}");
    }

    #[test]
    fn valid_graph_routes_to_graph_variant() {
        let content: DecisionContent = serde_json::from_str(r#"{"nodes":[],"edges":[]}"#).unwrap();
        assert!(content.as_graph().is_some());
    }
}
