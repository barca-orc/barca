//! The intermediate representation: the graph barca plans and runs, as a frontend hands it over.
//!
//! A frontend reads a project in some language and produces nodes; the graph, the planner and
//! the scheduler read nodes and never the frontend. The only frontend today is the Python one
//! (`barca_compile`: `parse`, `cone`, `definition`, `link`). `docs/engineering/ir.md` has the
//! field-by-field account and what a second frontend would have to supply.
//!
//! A node ([`DagNode`]) holds four things, and the types keep them apart:
//!
//! - its **declaration** ([`NodeDecl`]): what a scheduler needs, in no language;
//! - its **fingerprint** ([`Fingerprint`]): the definition hash and its named components,
//!   computed by the frontend and never interpreted here;
//! - its **invocation** ([`Invocation`]): how a worker runs the step, which is the frontend's
//!   business (for Python: a file, a function name and how its frames are loaded);
//! - the frontend's **findings** ([`NodeFindings`]): what its analysis noticed, reported as
//!   plan warnings and part of no hash.
//!
//! The graph itself (`barca_model::dag::Dag`: these nodes, their edges, the groups) is not in
//! this crate yet because it needs petgraph; it uses only this module and node names, and its
//! serializable form is `barca_model::dag::DagParts`. The values a declaration is made of
//! ([`NodeKind`], [`Freshness`], [`PartitionSpec`], [`SinkDecl`], [`BatchSpec`], ...) are
//! defined in [`crate::model`] and re-exported here, so this module lists everything a
//! `barca-ir` crate would hold.
//!
//! The serde derives are internal (a round-trip test, `crates/barca-service/tests/ir_roundtrip.rs`).
//! No external format exists: nothing reads or writes this as a file.

mod group;
mod node;

pub use group::NodeGroup;
pub use node::{
    DagNode, EdgeKind, Fingerprint, Invocation, NodeDecl, NodeFindings, PythonFunction,
};

pub use crate::model::{
    BatchSpec, CronExpr, Freshness, NodeKind, NodeRef, PartitionSpec, PartitionValue,
    SerializerKind, SinkDecl, ValueType,
};
