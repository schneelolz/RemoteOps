//! `RemoteOps` 领域对象和值对象。

mod capability;
mod connection;
mod environment;
mod error;
mod event;
mod identifiers;
mod lease;
mod permission;
mod target_context;
mod visual;
mod visual_wait;

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
    AgentBootId, AgentInstanceId, ApprovalId, ControllerInstanceId, ControllerOwnerId,
    FileTransferId, PairingCode, RequestId, SerialSessionId, SessionId, ShellId,
};
pub use lease::PairingLease;
pub use permission::PermissionMode;
pub use target_context::{
    AgentRunMode, CapabilityAvailability, GuidanceReference, MAX_TARGET_CONTEXT_BYTES,
    TARGET_CONTEXT_SCHEMA_VERSION, TargetCapabilityStatus, TargetContext, TargetGuidance,
    TargetRuntimeFacts,
};
pub use visual::{
    VisualActionResult, VisualCapability, VisualDisplay, VisualObservation, VisualSessionState,
    VisualTarget, VisualWindow,
};
pub use visual_wait::{
    DEFAULT_VISUAL_WAIT_TIMEOUT_MILLIS, MAX_VISUAL_WAIT_CONDITION_BYTES,
    MAX_VISUAL_WAIT_TIMEOUT_MILLIS, VisualWaitCondition, normalize_visual_wait_timeout,
};
