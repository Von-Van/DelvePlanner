//! The link between the app and the Python agent (DelveAgent piece D).
//!
//! The agent runs as a child process the app supervises. It speaks versioned newline-delimited
//! JSON on stdin/stdout (`protocol`), reaches the user's local model only through the `broker`,
//! and reaches planner data only through read tools the app answers. It never opens the planner
//! databases, never sees calendar credentials, and never writes anything. The design, the message
//! list, and the packaging decision are in `docs/delveagent/sidecar.md`.

pub mod broker;
pub mod protocol;
pub mod supervisor;

pub use broker::ModelBroker;
pub use protocol::{ErrorCode, LinkError, MAX_MESSAGE_BYTES, PROTOCOL_VERSION};
pub use supervisor::{
    AgentInfo, RunBudget, RunOutcome, RunRequest, Sidecar, SidecarCommand, SidecarError,
    SidecarHost, SidecarLimits, StopReason,
};
