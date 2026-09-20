//! Reusable scenarios: the registry with its lifecycle rules, and simulation
//! probes that let a caller develop and test a world before spending an
//! investigation.

pub mod probe;
pub mod store;

pub use probe::{
    MAX_PROBE_CALLS, ProbeCall, ProbeProgress, ProbeRequest, ProbeStatus, ProbeStopReason,
    ProbeTarget, run_probe, runtime_for,
};
pub use store::{
    DeleteOutcome, InvestigationRef, ScenarioRecord, ScenarioStore, StoreError, WorkspaceAction,
};
