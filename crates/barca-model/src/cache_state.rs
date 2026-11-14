//! The cache state of a node: `cache.state` in `barca status --json`, the server's node state
//! and the `--state` filter. One vocabulary, spelled in JSON exactly as [`CacheState::as_str`].

use serde::{Deserialize, Deserializer, Serialize};

/// A node's cache state (`barca docs status`, the state table).
///
/// Producers set a variant; [`FromStr`](std::str::FromStr) (`--state`, `?state=`) is strict.
/// Decoding JSON is lenient: a state this build does not know (a newer server's, the contract
/// adds enum values) decodes as [`CacheState::Unknown`], "cannot say what `get` would do",
/// and its `reason` and `detail` still say why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum CacheState {
    /// A successful result matches this code and these inputs; `get` serves it.
    Cached,
    /// Cached, but not where it should be (local copy, store object, digest or server ledger).
    Unsynced,
    /// It ran before, but `get` would run it again.
    Stale,
    /// No successful materialization is recorded.
    NeverRun,
    /// A partitioned asset with some keys cached.
    Partial,
    /// A partitioned asset whose missing keys wait on failed keys of an input.
    Blocked,
    /// Not decidable before something upstream runs (dynamic partitions, sensor output, cutoff).
    Unknown,
    /// Tasks and sensors are never cached.
    AlwaysRuns,
}

impl CacheState {
    /// Every state, in the order the docs, the OpenAPI enum and `summary` list them.
    pub const ALL: [CacheState; 8] = [
        CacheState::Cached,
        CacheState::Unsynced,
        CacheState::Stale,
        CacheState::NeverRun,
        CacheState::Partial,
        CacheState::Blocked,
        CacheState::Unknown,
        CacheState::AlwaysRuns,
    ];

    /// The JSON spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            CacheState::Cached => "cached",
            CacheState::Unsynced => "unsynced",
            CacheState::Stale => "stale",
            CacheState::NeverRun => "never_run",
            CacheState::Partial => "partial",
            CacheState::Blocked => "blocked",
            CacheState::Unknown => "unknown",
            CacheState::AlwaysRuns => "always_runs",
        }
    }
}

impl<'de> Deserialize<'de> for CacheState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = std::borrow::Cow::<'de, str>::deserialize(deserializer)?;
        Ok(raw.parse().unwrap_or(CacheState::Unknown))
    }
}

impl std::fmt::Display for CacheState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The JSON spelling is not one of the states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCacheState(pub String);

impl std::fmt::Display for UnknownCacheState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown cache state '{}'", self.0)
    }
}

impl std::error::Error for UnknownCacheState {}

impl std::str::FromStr for CacheState {
    type Err = UnknownCacheState;

    /// Parses the JSON spelling exactly (`never_run`, not `never-run`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        CacheState::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or_else(|| UnknownCacheState(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_round_trips_through_str_and_serde() {
        for state in CacheState::ALL {
            assert_eq!(state.as_str().parse::<CacheState>(), Ok(state));
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, format!("\"{}\"", state.as_str()));
            assert_eq!(serde_json::from_str::<CacheState>(&json).unwrap(), state);
            assert_eq!(state.to_string(), state.as_str());
        }
        assert!("never-run".parse::<CacheState>().is_err());
        assert!("will_run".parse::<CacheState>().is_err());
    }

    #[test]
    fn a_state_from_a_newer_producer_decodes_as_unknown() {
        let state: CacheState = serde_json::from_str("\"future_state\"").unwrap();
        assert_eq!(state, CacheState::Unknown);
        assert!("future_state".parse::<CacheState>().is_err());
        assert!(serde_json::from_str::<CacheState>("1").is_err());
    }
}
