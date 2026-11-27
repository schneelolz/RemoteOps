//! 已认证人工桌面的限时 TURN REST 凭据签发。
use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use remoteops_protocol::desktop::rtc::TurnGrant;
use ring::hmac;
use serde::Deserialize;
use std::{net::Ipv4Addr, path::Path};

pub(crate) struct Issuer {
    address: Ipv4Addr,
    port: u16,
    key: hmac::Key,
}

impl Issuer {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Config {
            address: Ipv4Addr,
            port: u16,
            secret: String,
        }
        ensure!(std::fs::metadata(path)?.len() <= 4096, "TURN 签发配置过大");
        let c: Config = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|_| anyhow::anyhow!("TURN 签发配置格式无效"))?;
        ensure!(
            c.port > 0 && c.secret.len() == 64 && c.secret.bytes().all(|b| b.is_ascii_hexdigit()),
            "TURN 签发配置无效"
        );
        Ok(Self {
            address: c.address,
            port: c.port,
            key: hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, c.secret.as_bytes()),
        })
    }

    pub(crate) fn grant(&self) -> TurnGrant {
        let expires = chrono::Utc::now().timestamp() + 3600;
        let username = format!("{expires}:{}", uuid::Uuid::new_v4().simple());
        TurnGrant {
            address: self.address,
            port: self.port,
            expires,
            password: STANDARD.encode(hmac::sign(&self.key, username.as_bytes()).as_ref()),
            username,
        }
    }
}
