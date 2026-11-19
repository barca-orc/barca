//! The typed views a host answers with (#1056, B2 and B3): one definition per view, computed
//! here. `barca-server` and `barca-cli` fill the inputs (a server adds what only it holds, as
//! a plain-data overlay) and render the result; the UI reads the generated TypeScript of the
//! same structs (`ui/src/lib/generated/`).
//!
//! Each struct is the JSON a route serves today, key for key. Where a concept is served in two
//! shapes (a schedule's times as epoch seconds here and RFC 3339 in
//! [`crate::status::ScheduleStatus`]), both stay as they are: this module types what is
//! served, it does not pick one.

mod pauses;
mod queue;
mod runs;
mod schedule;
mod server;

pub use pauses::{PauseRequest, Pauses, Pins};
pub use queue::{QueueEntry, QueueView, RecordedRun, recorded as recorded_queue};
pub use runs::{
    ActiveStep, FirstFailure, Flight, LiveRun, LiveStatus, LiveSteps, PartitionProgress,
    RunCancelling, RunDraft, RunHandle, RunList, RunListRequest, RunLogs, RunSummary,
    RunViewRequest, RunViewStatus, RunViewed, RunsQuery, in_flight, in_flight_list, in_flight_now,
    run_list, run_view,
};
pub use schedule::{JobSkips, ScheduleJob, SchedulePauseAnswer, SkipReason, schedule_jobs};
pub use server::{
    AuthMode, AuthStatus, CacheSize, Capabilities, HealthCapacity, HealthQueue, HealthStatus,
    Project, ProjectFile, ProjectTelemetry, ServerHealth,
};
