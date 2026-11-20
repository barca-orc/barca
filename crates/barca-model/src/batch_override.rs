//! The per-run `--batch` value (RFC-0013): `--batch V` and `--batch node=V`, and its grammar.
//!
//! A leaf: it reads no source and no graph, only a node it is asked about. The planning side
//! checks it against the graph (`targets`, [`BatchOverride::check`]) and the scheduler applies
//! it (`barca_engine::batch`); the Python frontend validates `Schedule(batch=...)` with the
//! same grammar (`parse`), which is why it is not the frontend's.

/// One `--batch` value: one key per call, at most N, or all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideValue {
    One,
    Size(u32),
    All,
}

impl OverrideValue {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "none" | "None" | "1" => Ok(OverrideValue::One),
            "all" => Ok(OverrideValue::All),
            n => match n.parse::<u32>() {
                Ok(n) if n >= 1 => Ok(OverrideValue::Size(n)),
                _ => Err(format!(
                    "--batch value '{value}' is not none, all or a whole number of at least 1"
                )),
            },
        }
    }

    fn as_str(self) -> String {
        match self {
            OverrideValue::One => "none".to_string(),
            OverrideValue::Size(n) => n.to_string(),
            OverrideValue::All => "all".to_string(),
        }
    }
}

/// `--batch V` and `--batch node=V` (repeatable) of one run: what replaces the declared
/// `batch=` of the nodes that declare one. A node without `batch=` takes a single key and is
/// never batched by an override.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchOverride {
    /// A bare `--batch V`: every node that declares `batch=`.
    pub all: Option<OverrideValue>,
    /// `--batch node=V`, by the name as written (a function name, `name=`, or a full node id).
    pub nodes: Vec<(String, OverrideValue)>,
}

impl BatchOverride {
    /// Parse the values of `--batch` (each `V` or `node=V`). A usage error names the value.
    pub fn parse(items: &[String]) -> Result<Self, String> {
        let mut out = BatchOverride::default();
        for item in items {
            match item.rsplit_once('=') {
                Some((node, value)) if !node.trim().is_empty() => {
                    out.nodes
                        .push((node.trim().to_string(), OverrideValue::parse(value)?));
                }
                _ => {
                    if out.all.is_some() {
                        return Err(format!(
                            "--batch is given twice without a node name ('{item}'); name the \
                             node of the second, as --batch node={item}"
                        ));
                    }
                    out.all = Some(OverrideValue::parse(item)?);
                }
            }
        }
        Ok(out)
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_none() && self.nodes.is_empty()
    }

    /// Back to the flag's values (for a run sent to a server).
    pub fn to_flags(&self) -> Vec<String> {
        let mut out: Vec<String> = self.all.iter().map(|v| v.as_str()).collect();
        out.extend(
            self.nodes
                .iter()
                .map(|(n, v)| format!("{n}={}", v.as_str())),
        );
        out
    }

    pub fn value_for(&self, node: &crate::model::DagNode) -> Option<OverrideValue> {
        self.nodes
            .iter()
            .rev()
            .find(|(name, _)| names_node(node, name))
            .map(|(_, v)| *v)
            .or(self.all)
    }
}

pub fn names_node(node: &crate::model::DagNode, name: &str) -> bool {
    node.id == name
        || node.function_name() == name
        || node.decl.explicit_name.as_deref() == Some(name)
        || crate::names::short_name(&node.id) == name
}

/// `--batch node=V` against the graph, as `barca_compile::targets::validate_refresh_names` is
/// for `--refresh`.
impl BatchOverride {
    /// Every named node must be in the graph and declare `batch=`. A usage error otherwise.
    pub fn check(&self, dag: &crate::dag::Dag) -> Result<(), String> {
        for (name, _) in &self.nodes {
            let found: Vec<&crate::model::DagNode> = dag
                .graph
                .node_weights()
                .filter(|n| names_node(n, name))
                .collect();
            let Some(node) = found.first() else {
                return Err(format!(
                    "--batch {name}=...: no node named '{name}' in the loaded pipeline"
                ));
            };
            if node.decl.batch.is_none() {
                return Err(format!(
                    "--batch {name}=...: '{name}' does not declare batch=, so its function takes \
                     a single key; add batch= to it to batch it (barca docs partitions, \
                     \"Processing many keys per call\")"
                ));
            }
        }
        Ok(())
    }
}
