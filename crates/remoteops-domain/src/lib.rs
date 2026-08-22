//! `RemoteOps` 领域对象和值对象。

mod capability;
mod connection;
mod environment;
mod error;
mod event;
mod identifiers;
mod lease;
mod permission;

pub use capability::{Capability, CapabilitySet, ShellKind};
pub use connection::{ConnectionDescriptor, ConnectionState, SessionRole};
pub use environment::{
    ENVIRONMENT_PROFILE_SCHEMA_VERSION, EnvironmentProfile, ShellProfile, ToolProfile,
};
pub use error::DomainError;
pub use event::{
    ApprovalState, AuditEvent, EventPayload, EventSource, PowerAction, RemoteEvent,
    RemoteOperation, SerialDataBits, SerialFlowControl, SerialLineEnding, SerialParity,
    SerialSettings, SerialStopBits, SerialTerminalProfile, ServiceAction,
};
pub use identifiers::{
    AgentInstanceId, ApprovalId, ControllerInstanceId, ControllerOwnerId, FileTransferId,
    PairingCode, RequestId, SerialSessionId, SessionId, ShellId,
};
pub use lease::PairingLease;
pub use permission::PermissionMode;

