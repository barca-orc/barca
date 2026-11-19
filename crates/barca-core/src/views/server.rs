//! What a server says of itself: `GET /health`, `GET /project`, `GET /auth/status`. The host
//! fills every field (none is computed from the metadata database); the struct is the wire.

use crate::load::LoadError;
use serde::{Deserialize, Serialize};

/// `status` of `GET /health`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
    /// The server was told to stop: it finishes its runs and starts none.
    Draining,
}

/// `capabilities` of `GET /health`: what a client may rely on beyond the API version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Capabilities {
    /// The version of event-stream resume (`Last-Event-ID`) the server speaks.
    pub event_resume: u32,
}

/// `cache` of `GET /health`: `.barca/` against its size limit, as last measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CacheSize {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub limit_bytes: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub size_bytes: u64,
}

/// `queue.capacity` of `GET /health`: the capacity every run shares, in brief.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HealthCapacity {
    pub in_use: u32,
    pub total: u32,
    /// How many steps wait for weight to come free.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub waiting: usize,
}

/// `queue` of `GET /health`: the run queue in brief; `GET /queue` has the runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HealthQueue {
    /// The capacity every run shares (`--capacity`); `null` when the server has none.
    pub capacity: Option<HealthCapacity>,
    /// How many runs execute at once; `null` from a `--read-only` server whose records do
    /// not say.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub max_runs: Option<usize>,
    pub paused: bool,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub queued: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub running: usize,
}

/// `GET /health`: liveness (or `draining`), the versions a client checks, the environment and
/// artifact store the server uses, and what it can do.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ServerHealth {
    /// Major version of the HTTP API; a client refuses another.
    pub api_version: u32,
    /// `.barca/` against its size limit, as last measured after a run or on the minute
    /// checkpoint (RFC-0012); `null` before the first measurement, with the limit off and
    /// from a `--read-only` server.
    pub cache: Option<CacheSize>,
    pub capabilities: Capabilities,
    /// The environment this server runs (`barca serve --env`).
    pub env: String,
    /// The served graph is older than the sources on disk; `null` while that is not known.
    pub graph_stale: Option<bool>,
    /// Unloaded sources and graph-invalid definitions; healthy node routes remain available.
    pub load_errors: Vec<LoadError>,
    /// Why the metadata DB cannot be used, while it cannot; `null` otherwise.
    pub metadata_error: Option<String>,
    pub queue: HealthQueue,
    /// `barca serve --read-only`: run and cancel endpoints are refused.
    pub read_only: bool,
    /// Whether this server fires `Schedule(...)` nodes.
    pub scheduler: bool,
    /// `scheduler`, and false once the scheduler stopped on an error.
    pub scheduler_enabled: bool,
    /// When this server started (RFC 3339): a client that reconnects tells a restart by it.
    pub started_at: String,
    pub status: HealthStatus,
    /// The artifact store this server writes to; `null` when artifacts stay on its machine.
    pub store_uri: Option<String>,
    pub version: String,
}

/// `telemetry` of `GET /project`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ProjectTelemetry {
    /// Where a step's `trace_id` opens (`{datadog_url}/apm/trace/{trace_id}`): `null` when no
    /// Datadog site is configured and the integration is off.
    pub datadog_url: Option<String>,
}

/// One deployed file of `GET /project`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ProjectFile {
    /// Relative to `root` when it is inside it.
    pub path: String,
    pub sha256: String,
}

/// `GET /project`: the deployed tree. The files are hashed on each request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Project {
    pub api_version: u32,
    /// The commit `root` is at, when it is a git checkout.
    pub commit: Option<String>,
    /// When the server started (RFC 3339, UTC).
    pub deployed_at: String,
    pub env: String,
    pub files: Vec<ProjectFile>,
    pub root: Option<String>,
    pub telemetry: ProjectTelemetry,
}

/// `mode` of `GET /auth/status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Every request is accepted.
    Off,
    /// Requests need a project token.
    Token,
}

/// `GET /auth/status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AuthStatus {
    pub mode: AuthMode,
}
