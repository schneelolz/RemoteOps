//! Strict, versioned MCP onboarding wire format. Grants are secrets, never log them.
use std::{fmt, io::Cursor, net::IpAddr};

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use remoteops_domain::ControllerOwnerId;
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

/// Maximum encoded setup input, including certificates.
pub const MAX_SETUP_BYTES: usize = 128 * 1024;
/// Versioned code prefix. A file contains the exact same JSON representation.
pub const SETUP_CODE_PREFIX: &str = "remoteops-setup-v1.";
/// Deployment-wide advertised endpoints, explicitly set by an administrator.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdvertisedEndpoints {
    pub enrollment_url: String,
    pub relay: String,
    pub server_name: String,
    pub relay_ca_pem: Option<String>,
    pub enrollment_ca_pem: Option<String>,
}

impl AdvertisedEndpoints {
    /// Rejects ambiguous endpoints, non-HTTPS enrollment and non-certificate trust.
    /// # Errors
    /// Returns a non-secret validation error for invalid endpoints or trust material.
    pub fn validate(&self) -> anyhow::Result<()> {
        let url = Url::parse(&self.enrollment_url).context("登记地址格式无效")?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host_str().is_none()
            || url.path() != "/api/mcp/enroll"
            || url.port() == Some(0)
            || self.enrollment_url.chars().any(char::is_whitespace)
        {
            bail!("登记地址必须是 HTTPS /api/mcp/enroll 地址，不能包含账户、查询或片段");
        }
        validate_host(url.host_str().context("缺少登记服务器名称")?)?;
        let relay = Url::parse(&format!("tls://{}", self.relay)).context("Relay 地址格式无效")?;
        if relay.host_str().is_none()
            || relay.port().is_none_or(|port| port == 0)
            || !relay.username().is_empty()
            || relay.password().is_some()
            || !relay.path().is_empty()
            || relay.query().is_some()
            || relay.fragment().is_some()
            || self.relay.chars().any(char::is_whitespace)
        {
            bail!("Relay 地址必须是明确的 host:port，不能使用监听通配地址");
        }
        validate_host(relay.host_str().context("缺少 Relay 服务器名称")?)?;
        validate_host(&self.server_name)?;
        rustls::pki_types::ServerName::try_from(self.server_name.clone())
            .map_err(|_| anyhow::anyhow!("TLS 服务器名称无效"))?;
        for pem in [&self.relay_ca_pem, &self.enrollment_ca_pem]
            .into_iter()
            .flatten()
        {
            validate_certificate_pem(pem)?;
        }
        Ok(())
    }
}

fn validate_host(host: &str) -> anyhow::Result<()> {
    let normalized = host.trim_matches(['[', ']']);
    if normalized.is_empty()
        || normalized.contains('*')
        || normalized.chars().any(char::is_whitespace)
    {
        bail!("服务器名称不能为空或使用通配地址");
    }
    if let Ok(ip) = normalized.parse::<IpAddr>()
        && (ip.is_unspecified() || ip.is_multicast())
    {
        bail!("服务器名称不能使用监听通配地址或组播地址");
    }
    Ok(())
}

/// Validate public certificate trust material without accepting keys or arbitrary payloads.
/// # Errors
/// Returns an error unless every PEM item is an X.509 certificate.
pub fn validate_certificate_pem(pem: &str) -> anyhow::Result<()> {
    if pem.len() > 32 * 1024 || pem.contains("PRIVATE KEY") {
        bail!("信任配置只能包含有界的公开 PEM 证书，不能包含私钥");
    }
    let mut roots = rustls::RootCertStore::empty();
    let mut remainder = pem.trim();
    while !remainder.is_empty() {
        let Some(body) = remainder.strip_prefix("-----BEGIN CERTIFICATE-----") else {
            bail!("信任配置只能包含公开 PEM 证书");
        };
        let Some((_, tail)) = body.split_once("-----END CERTIFICATE-----") else {
            bail!("PEM 证书未结束");
        };
        let consumed = remainder.len() - tail.len();
        let block = &remainder[..consumed];
        let cert = rustls_pemfile::certs(&mut Cursor::new(block))
            .next()
            .context("缺少 PEM 证书")?
            .context("PEM 证书无效")?;
        roots.add(cert).context("X.509 证书无效")?;
        remainder = tail.trim();
    }
    if roots.is_empty() {
        bail!("PEM 证书不能为空");
    }
    Ok(())
}

/// One-time setup input. `Debug` deliberately redacts the secret.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetupDocument {
    pub version: u32,
    pub grant_id: Uuid,
    pub grant_secret: String,
    pub enrollment_url: String,
    pub relay: String,
    pub server_name: String,
    pub relay_ca_pem: Option<String>,
    pub enrollment_ca_pem: Option<String>,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for SetupDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetupDocument")
            .field("grant_id", &self.grant_id)
            .field("grant_secret", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl SetupDocument {
    #[must_use]
    pub fn endpoints(&self) -> AdvertisedEndpoints {
        AdvertisedEndpoints {
            enrollment_url: self.enrollment_url.clone(),
            relay: self.relay.clone(),
            server_name: self.server_name.clone(),
            relay_ca_pem: self.relay_ca_pem.clone(),
            enrollment_ca_pem: self.enrollment_ca_pem.clone(),
        }
    }
    /// Validates the format, not wall-clock expiry; only the server decides first redemption.
    /// # Errors
    /// Returns an error for unsupported or malformed setup inputs.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.version != 1 || self.grant_id.is_nil() {
            bail!("不支持的安装配置版本或标识");
        }
        validate_secret(&self.grant_secret)?;
        self.endpoints().validate()
    }
    /// # Errors
    /// Returns an error if this document is invalid or cannot be encoded.
    pub fn encode_code(&self) -> anyhow::Result<String> {
        self.validate()?;
        Ok(format!(
            "{SETUP_CODE_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }
    /// Reads JSON file contents or the equivalent setup code; errors never quote input.
    /// # Errors
    /// Returns an error for oversized, unsupported or malformed input.
    pub fn decode(input: &str) -> anyhow::Result<Self> {
        if input.len() > MAX_SETUP_BYTES {
            bail!("安装配置过大");
        }
        let input = input.trim().trim_start_matches('\u{feff}');
        let bytes = if let Some(code) = input.strip_prefix(SETUP_CODE_PREFIX) {
            URL_SAFE_NO_PAD
                .decode(code)
                .map_err(|_| anyhow::anyhow!("安装码格式无效"))?
        } else {
            input.as_bytes().to_vec()
        };
        let document: Self =
            serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("安装配置格式无效"))?;
        document.validate()?;
        Ok(document)
    }
}

/// # Errors
/// A setup or installation secret must contain exactly 256 bits in canonical base64url form.
pub fn validate_secret(secret: &str) -> anyhow::Result<()> {
    let bytes = URL_SAFE_NO_PAD
        .decode(secret)
        .map_err(|_| anyhow::anyhow!("凭据格式无效"))?;
    if bytes.len() != 32 || URL_SAFE_NO_PAD.encode(&bytes) != secret {
        bail!("凭据格式无效");
    }
    Ok(())
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RedeemRequest {
    pub grant_id: Uuid,
    pub grant_secret: String,
    pub installation_id: Uuid,
    pub installation_secret: String,
}
impl fmt::Debug for RedeemRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedeemRequest")
            .field("secrets", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RedeemResponse {
    pub installation_id: Uuid,
    pub owner_id: ControllerOwnerId,
    pub relay: String,
    pub server_name: String,
    pub relay_ca_pem: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateGrantRequest {
    pub client_name: Option<String>,
    pub expires_in_hours: Option<u32>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct CreateGrantResponse {
    pub setup: SetupDocument,
    pub setup_code: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GrantSummary {
    pub grant_id: Uuid,
    pub client_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub state: String,
    pub installation_id: Option<Uuid>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ClientSummary {
    pub installation_id: Uuid,
    pub client_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen: Option<DateTime<Utc>>,
    pub revoked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> SetupDocument {
        SetupDocument {
            version: 1,
            grant_id: Uuid::new_v4(),
            grant_secret: URL_SAFE_NO_PAD.encode([7; 32]),
            enrollment_url: "https://relay.example.test/api/mcp/enroll".into(),
            relay: "relay.example.test:7443".into(),
            server_name: "relay.example.test".into(),
            relay_ca_pem: None,
            enrollment_ca_pem: None,
            expires_at: Utc::now(),
        }
    }
    #[test]
    fn code_and_file_roundtrip_redact_debug() {
        let setup = fixture();
        for input in [
            serde_json::to_string(&setup).unwrap(),
            setup.encode_code().unwrap(),
        ] {
            let decoded = SetupDocument::decode(&input).unwrap();
            assert_eq!(decoded.grant_id, setup.grant_id);
            assert_eq!(decoded.grant_secret, setup.grant_secret);
            assert!(!format!("{decoded:?}").contains(&setup.grant_secret));
        }
    }
    #[test]
    fn malicious_schema_and_endpoints_fail_closed() {
        let setup = fixture();
        let mut value = serde_json::to_value(&setup).unwrap();
        value["controller_token"] = "not-allowed".into();
        assert!(SetupDocument::decode(&value.to_string()).is_err());
        for url in [
            "http://relay.example.test/api/mcp/enroll",
            "https://u:p@relay.example.test/api/mcp/enroll",
            "https://relay.example.test/api/mcp/enroll?secret=x",
            "https://0.0.0.0/api/mcp/enroll",
            "https://relay.example.test/other",
        ] {
            let mut bad = fixture();
            bad.enrollment_url = url.into();
            assert!(bad.validate().is_err(), "{url}");
        }
        for relay in [
            "0.0.0.0:7443",
            "[::]:7443",
            "host",
            "host:0",
            "host:7443/path",
            "host:7443?x=1",
            "user@host:7443",
        ] {
            let mut bad = fixture();
            bad.relay = relay.into();
            assert!(bad.validate().is_err(), "{relay}");
        }
    }
    #[test]
    fn certificates_are_public_valid_and_only_pem() {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        assert!(validate_certificate_pem(&cert.cert.pem()).is_ok());
        for bad in [
            String::new(),
            cert.signing_key.serialize_pem(),
            "-----BEGIN CERTIFICATE-----\nbogus\n-----END CERTIFICATE-----".into(),
            format!("{}\necho pwned", cert.cert.pem()),
        ] {
            assert!(validate_certificate_pem(&bad).is_err());
        }
    }
    #[test]
    fn expiry_is_server_authoritative_for_safe_retry() {
        let mut setup = fixture();
        setup.expires_at = Utc::now() - chrono::Duration::days(8);
        assert!(setup.validate().is_ok());
        setup.version = 2;
        assert!(setup.validate().is_err());
    }
}
