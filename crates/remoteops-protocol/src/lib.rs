//! Agent、Relay 和 Controller 共用的版本化线协议。

mod codec;
mod credential;
mod message;
mod tls;

pub use codec::{FrameError, MAX_FRAME_SIZE, read_frame, write_frame};
pub use credential::{
    CredentialEncryptionContext, CredentialEncryptionError, CredentialEncryptionKeyPair,
    EncryptedCredentialPayload, open_credential, seal_credential,
};
pub use message::{
    AgentHello, AgentLeaseRenewed, AgentPermissionModeChanged, AgentResumeCommitAck,
    AgentResumeCommitted, AgentShutdownRequest, AgentShutdownResult, AgentWelcome, AgentWelcomeAck,
    ApprovalDecision, ApprovalRequest, ApprovalResult, AuthorizedRemoteRequest, ClientHello,
    ControllerBinding, ControllerControlMode, ControllerControlModeUpdate, ControllerHello,
    ControllerKind, PROTOCOL_VERSION, PairRequest, PairResult, RelayAuthorization,
    ReleaseSessionRequest, ReleaseSessionResult, RemoteRequest, RemoteResponse, WireMessage,
};
pub use tls::{
    TlsCertificateProbe, TlsError, TlsServerConfig, accept_tls, connect_tls, load_client_config,
    load_native_client_config, load_pinned_client_config, load_server_config,
    normalize_certificate_fingerprint, probe_server_certificate,
};
