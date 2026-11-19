//! Groups: organizational metadata over nodes, separate from dependency edges and hashes.

use serde::{Deserialize, Serialize};

// A named set of nodes (`group(...)`), by node id. `output` is the member the group stands
// for; a member may be another group's id. A `//` comment on purpose: ts-rs copies doc comments
// into the generated UI type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct NodeGroup {
    pub id: String,
    pub name: String,
    pub description: String,
    pub members: Vec<String>,
    pub output: String,
}
