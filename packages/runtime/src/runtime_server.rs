//! Host-agnostic Runtime Server ownership primitives.
//!
//! Transport adapters own sockets and processes. This module owns only the
//! Session/AgentRun state transitions that every local host must share.

#[path = "runtime_server/lease.rs"]
mod lease;
#[path = "runtime_server/live_text.rs"]
mod live_text;

pub use lease::{
    ActiveAgentRun, AgentRunLease, AgentRunRegistry, OwnerExitDisposition, RuntimeClientKind,
    StartAgentRunError,
};
pub use live_text::{LiveTextJournal, LiveTextJournalKey, LiveTextOperation};
