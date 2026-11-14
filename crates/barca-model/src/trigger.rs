//! What started a run ([`Trigger`]): recorded on its `runs` row and its `barca.run` span, and
//! part of the server's run contract (`barca_core::outcome` re-exports it).

use crate::time::rfc3339;
use serde::{Deserialize, Serialize};

/// What started a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum TriggeredBy {
    /// A cron tick of `barca serve`.
    Schedule,
    /// An HTTP request to `barca serve` (`POST /runs`, the web UI, the older trigger routes).
    Api,
    /// A run a laptop computed and posted to the server's ledger.
    Client,
    /// A command typed on this machine (`barca get`, `barca run`).
    #[default]
    Manual,
}

impl TriggeredBy {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggeredBy::Schedule => "schedule",
            TriggeredBy::Api => "api",
            TriggeredBy::Client => "client",
            TriggeredBy::Manual => "manual",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "schedule" => TriggeredBy::Schedule,
            "api" => TriggeredBy::Api,
            "client" => TriggeredBy::Client,
            "manual" => TriggeredBy::Manual,
            _ => return None,
        })
    }
}

/// How a run was started, recorded on its `runs` row and its `barca.run` span.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    pub triggered_by: TriggeredBy,
    /// For a scheduled run: the tick it was started for, RFC 3339 in UTC.
    pub scheduled_at: Option<String>,
    /// For a scheduled run: the schedule's id, the full id of the scheduled node.
    pub schedule_id: Option<String>,
}

impl Trigger {
    /// A run started over HTTP.
    pub fn api() -> Self {
        Self {
            triggered_by: TriggeredBy::Api,
            ..Self::default()
        }
    }

    /// A run started by the cron tick of `schedule_id` at `epoch` (unix seconds).
    pub fn schedule(schedule_id: &str, epoch: i64) -> Self {
        Self {
            triggered_by: TriggeredBy::Schedule,
            scheduled_at: Some(rfc3339(epoch)),
            schedule_id: Some(schedule_id.to_string()),
        }
    }
}
