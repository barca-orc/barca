//! `GET /pauses` and `GET /pins`: what an operator holds back, and what is pinned.

use crate::db::{AssetPause, AssetPin};
use crate::pauses::PausedSchedule;
use serde::{Deserialize, Serialize};

/// `GET /pauses`, and the answer of `POST /pauses` (with `added`) and `POST /pauses/resume`
/// (with `removed`): every paused node and every paused schedule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Pauses {
    /// `GET /pauses?target=` only (`barca pause TARGET --dry-run`): what a pause of that
    /// target would cover. Nothing is paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub covers: Option<crate::views::PauseCover>,
    /// `POST /pauses` only: the ids it paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub added: Option<Vec<String>>,
    /// Every paused node, with when and through what it was paused.
    #[serde(default)]
    pub paused: Vec<AssetPause>,
    /// `POST /pauses/resume` only: the ids it resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub removed: Option<Vec<String>>,
    /// Every paused schedule (`barca pause --schedule`).
    #[serde(default)]
    pub schedules: Vec<PausedSchedule>,
    /// How many nodes are paused.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total: usize,
}

impl Pauses {
    pub fn of(paused: Vec<AssetPause>, schedules: Vec<PausedSchedule>) -> Self {
        Pauses {
            total: paused.len(),
            paused,
            schedules,
            covers: None,
            added: None,
            removed: None,
        }
    }
}

/// The body of `POST /pauses` and `POST /pauses/resume`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(deny_unknown_fields)]
pub struct PauseRequest {
    /// A node, named as a run target is, or a source file (`pipeline.py`): every node of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub target: Option<String>,
    /// Everything downstream of the target too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub downstream: bool,
    /// Resume only: every paused node.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub all: bool,
}

/// `GET /pins`: every pinned asset.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Pins {
    pub pins: Vec<AssetPin>,
}
