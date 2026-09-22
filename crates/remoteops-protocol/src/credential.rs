use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hpke::{
    Deserializable, Kem as KemTrait, OpModeR, OpModeS, Serializable, aead::ChaCha20Poly1305,
    kdf::HkdfSha256, kem::X25519HkdfSha256, setup_receiver, setup_sender,
};
use remoteops_domain::{AgentInstanceId, RequestId, SessionId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

type CredentialKem = X25519HkdfSha256;
type CredentialPrivateKey = <CredentialKem as KemTrait>::PrivateKey;
type CredentialPublicKey = <CredentialKem as KemTrait>::PublicKey;
type CredentialEncappedKey = <CredentialKem as KemTrait>::EncappedKey;

const CREDENTIAL_PAYLOAD_VERSION: u8 = 1;
const CREDENTIAL_INFO: &[u8] = b"remoteops/ssh-credential/hpke-v1";
const MAX_CREDENTIAL_BYTES: usize = 4096;

/// 与 SSH 密码密文绑定、由 MCP 和 Agent 独立重建的上下文。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CredentialEncryptionContext {
    pub protocol_version: u16,
    pub agent_instance_id: AgentInstanceId,
    pub session_id: SessionId,
    pub envelope_id: RequestId,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub command_sha256: String,
}

impl CredentialEncryptionContext {
    fn aad(&self) -> Result<Vec<u8>, CredentialEncryptionError> {
        serde_json::to_vec(self)
            .map_err(|error| CredentialEncryptionError::Encoding(error.to_string()))
    }
}

/// Relay 只需原样转发的版本化 HPKE 密码载荷。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EncryptedCredentialPayload {
    pub version: u8,
    pub key_id: String,
    pub envelope_id: RequestId,
    pub encapped_key_base64: String,
    pub ciphertext_base64: String,
}

/// Agent 当前进程使用的 HPKE 接收密钥；私钥不会序列化或落盘。
pub struct CredentialEncryptionKeyPair {
    private_key: CredentialPrivateKey,
    public_key_base64: String,
    key_id: String,
}

impl CredentialEncryptionKeyPair {
    #[must_use]
    pub fn generate() -> Self {
        let (private_key, public_key) = CredentialKem::gen_keypair();
        let public_key_bytes = public_key.to_bytes();
        Self {
            private_key,
            public_key_base64: BASE64.encode(public_key_bytes.as_slice()),
            key_id: format!("{:x}", Sha256::digest(public_key_bytes.as_slice())),
        }
    }

    #[must_use]
    pub fn public_key_base64(&self) -> &str {
        &self.public_key_base64
    }

    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

#[derive(Debug, Error)]
pub enum CredentialEncryptionError {
    #[error("SSH 密码不能为空")]
    Empty,
    #[error("SSH 密码超过 {MAX_CREDENTIAL_BYTES} 字节上限")]
    TooLarge,
    #[error("凭据加密公钥无效")]
    InvalidPublicKey,
    #[error("凭据密文格式无效")]
    InvalidPayload,
    #[error("凭据密文使用了过期或错误的 Agent 公钥")]
    KeyMismatch,
    #[error("凭据载荷与请求上下文不匹配")]
    ContextMismatch,
    #[error("凭据加密失败")]
    Seal,
    #[error("凭据解密或完整性校验失败")]
    Open,
    #[error("凭据编码失败：{0}")]
    Encoding(String),
    #[error("SSH 密码不是有效 UTF-8")]
    InvalidUtf8,
}

/// 使用 Agent 当前进程公钥加密单次 SSH 密码。
///
/// # Errors
///
/// 公钥、key ID、上下文或密码无效，或 HPKE 无法建立发送上下文时返回错误。
pub fn seal_credential(
    public_key_base64: &str,
    key_id: &str,
    context: &CredentialEncryptionContext,
    password: &[u8],
) -> Result<EncryptedCredentialPayload, CredentialEncryptionError> {
    validate_password(password)?;
    let public_key_bytes = BASE64
        .decode(public_key_base64)
        .map_err(|_| CredentialEncryptionError::InvalidPublicKey)?;
    let actual_key_id = format!("{:x}", Sha256::digest(&public_key_bytes));
    if actual_key_id != key_id {
        return Err(CredentialEncryptionError::KeyMismatch);
    }
    let public_key = CredentialPublicKey::from_bytes(&public_key_bytes)
        .map_err(|_| CredentialEncryptionError::InvalidPublicKey)?;
    let aad = context.aad()?;
    let (encapped_key, mut sender) = setup_sender::<ChaCha20Poly1305, HkdfSha256, CredentialKem>(
        &OpModeS::Base,
        &public_key,
        CREDENTIAL_INFO,
    )
    .map_err(|_| CredentialEncryptionError::Seal)?;
    let ciphertext = sender
        .seal(password, &aad)
        .map_err(|_| CredentialEncryptionError::Seal)?;
    Ok(EncryptedCredentialPayload {
        version: CREDENTIAL_PAYLOAD_VERSION,
        key_id: key_id.to_owned(),
        envelope_id: context.envelope_id,
        encapped_key_base64: BASE64.encode(encapped_key.to_bytes().as_slice()),
        ciphertext_base64: BASE64.encode(ciphertext),
    })
}

/// 使用 Agent 当前进程私钥解密并校验单次 SSH 密码。
///
/// # Errors
///
/// 密文版本、key ID、上下文、完整性或密码编码无效时返回错误。
pub fn open_credential(
    key_pair: &CredentialEncryptionKeyPair,
    context: &CredentialEncryptionContext,
    payload: &EncryptedCredentialPayload,
) -> Result<Zeroizing<String>, CredentialEncryptionError> {
    if payload.version != CREDENTIAL_PAYLOAD_VERSION {
        return Err(CredentialEncryptionError::InvalidPayload);
    }
    if payload.key_id != key_pair.key_id {
        return Err(CredentialEncryptionError::KeyMismatch);
    }
    if payload.envelope_id != context.envelope_id {
        return Err(CredentialEncryptionError::ContextMismatch);
    }
    let encapped_key_bytes = BASE64
        .decode(&payload.encapped_key_base64)
        .map_err(|_| CredentialEncryptionError::InvalidPayload)?;
    let encapped_key = CredentialEncappedKey::from_bytes(&encapped_key_bytes)
        .map_err(|_| CredentialEncryptionError::InvalidPayload)?;
    let ciphertext = BASE64
        .decode(&payload.ciphertext_base64)
        .map_err(|_| CredentialEncryptionError::InvalidPayload)?;
    if ciphertext.len() > MAX_CREDENTIAL_BYTES + 64 {
        return Err(CredentialEncryptionError::TooLarge);
    }
    let aad = context.aad()?;
    let mut receiver = setup_receiver::<ChaCha20Poly1305, HkdfSha256, CredentialKem>(
        &OpModeR::Base,
        &key_pair.private_key,
        &encapped_key,
        CREDENTIAL_INFO,
    )
    .map_err(|_| CredentialEncryptionError::Open)?;
    let plaintext = Zeroizing::new(
        receiver
            .open(&ciphertext, &aad)
            .map_err(|_| CredentialEncryptionError::Open)?,
    );
    validate_password(&plaintext)?;
    String::from_utf8(plaintext.to_vec())
        .map(Zeroizing::new)
        .map_err(|_| CredentialEncryptionError::InvalidUtf8)
}

fn validate_password(password: &[u8]) -> Result<(), CredentialEncryptionError> {
    if password.is_empty() {
        return Err(CredentialEncryptionError::Empty);
    }
    if password.len() > MAX_CREDENTIAL_BYTES {
        return Err(CredentialEncryptionError::TooLarge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> CredentialEncryptionContext {
        CredentialEncryptionContext {
            protocol_version: 15,
            agent_instance_id: AgentInstanceId::new(),
            session_id: SessionId::new(),
            envelope_id: RequestId::new(),
            host: "192.0.2.10".to_owned(),
            port: 22,
            username: "admin".to_owned(),
            command_sha256: "a".repeat(64),
        }
    }

    #[test]
    fn credential_round_trip_and_context_binding() {
        let key_pair = CredentialEncryptionKeyPair::generate();
        let context = context();
        let payload = seal_credential(
            key_pair.public_key_base64(),
            key_pair.key_id(),
            &context,
            b"secret-value",
        )
        .expect("凭据应加密");
        let opened = open_credential(&key_pair, &context, &payload).expect("凭据应解密");
        assert_eq!(opened.as_str(), "secret-value");

        let mut wrong_context = context.clone();
        wrong_context.host = "192.0.2.11".to_owned();
        assert!(open_credential(&key_pair, &wrong_context, &payload).is_err());
    }

    #[test]
    fn credential_tampering_and_wrong_key_are_rejected() {
        let key_pair = CredentialEncryptionKeyPair::generate();
        let context = context();
        let mut payload = seal_credential(
            key_pair.public_key_base64(),
            key_pair.key_id(),
            &context,
            b"secret-value",
        )
        .expect("凭据应加密");
        payload.ciphertext_base64.push('A');
        assert!(open_credential(&key_pair, &context, &payload).is_err());

        let other_key = CredentialEncryptionKeyPair::generate();
        assert!(open_credential(&other_key, &context, &payload).is_err());
    }
}
