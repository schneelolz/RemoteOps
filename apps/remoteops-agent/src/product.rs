use super::{
    AgentConfig, AgentFileConfig, Deserialize, Path, PathBuf, ProductProfile,
    active_agent_config_path, default_agent_config_path, infer_server_name,
    legacy_agent_config_path, sha256_bytes,
};
#[derive(Deserialize)]
pub(super) struct StoredAgentConfig {
    #[serde(default, deserialize_with = "default_control")]
    pub default_full_control: Option<bool>,
    #[serde(flatten)]
    pub config: AgentFileConfig,
}
fn default_control<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    bool::deserialize(d).map(Some)
}
impl AgentConfig {
    #[must_use]
    pub fn for_product(profile: &ProductProfile) -> Self {
        let (relay, name) = profile.endpoint.endpoint();
        let mut result = Self {
            default_full_control: profile.default_full_control,
            relay: relay.to_owned(),
            server_name: name.to_owned(),
            ..Self::default()
        };
        if !profile.legacy_remoteops_paths {
            result.state_file = profile.paths().state_file;
            result.transfer_root = profile.paths().transfer_root;
        }
        result
    }
    #[must_use]
    pub fn product_config_path(profile: &ProductProfile) -> PathBuf {
        if profile.legacy_remoteops_paths {
            active_agent_config_path()
        } else {
            profile.paths().active_config()
        }
    }
    /// Applies policy after input merging and invalidates trust when an endpoint changes.
    /// # Errors
    /// Rejects invalid product profiles and fixed endpoint overrides.
    pub fn resolve_product(
        mut self,
        profile: &ProductProfile,
        previous: Option<(&str, &str)>,
    ) -> anyhow::Result<Self> {
        profile.validate()?;
        self.relay = self.relay.trim().to_owned();
        self.server_name = self.server_name.trim().to_owned();
        if self.server_name.is_empty() && !self.relay.is_empty() {
            self.server_name = infer_server_name(&self.relay)?;
        }
        profile.endpoint.check(&self.relay, &self.server_name)?;
        let changed =
            previous.is_some_and(|p| p != (self.relay.as_str(), self.server_name.as_str()));
        if changed {
            self.ca_cert = None;
            self.tls_fingerprint = None;
        }
        if !profile.legacy_remoteops_paths || changed {
            self.isolate_state_file(profile);
        }
        Ok(self)
    }
    /// 按产品及已解析端点隔离身份路径，重复调用保持同一文件名。
    ///
    /// 调用前应通过 `resolve_product` 完成产品和端点校验；显式路径同样需要隔离。
    pub fn isolate_state_file(&mut self, profile: &ProductProfile) {
        let key = sha256_bytes(
            format!("{}\n{}\n{}", profile.id, self.relay, self.server_name).as_bytes(),
        );
        let stem = self
            .state_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("identity");
        let stem = stem
            .rsplit_once('-')
            .filter(|(_, hash)| hash.len() == 24 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .map_or(stem, |(stem, _)| stem);
        self.state_file = self
            .state_file
            .with_file_name(format!("{stem}-{}.json", &key[..24]));
    }

    /// Save atomically, falling back only for the implicit portable configuration location.
    /// # Errors
    /// Returns every policy or write failure without changing runtime preferences.
    pub fn save_for_product(
        &self,
        profile: &ProductProfile,
        path: &Path,
        allow_fallback: bool,
    ) -> anyhow::Result<PathBuf> {
        profile.endpoint.check(&self.relay, &self.server_name)?;
        let portable = if profile.legacy_remoteops_paths {
            default_agent_config_path()
        } else {
            profile.paths().portable_config
        };
        let fallback = if profile.legacy_remoteops_paths {
            legacy_agent_config_path()
        } else {
            profile.paths().user_config
        };
        persist_with_fallback(path, &portable, &fallback, allow_fallback, |path| {
            self.save_file(path)
        })
    }
}
fn persist_with_fallback(
    path: &Path,
    portable: &Path,
    fallback: &Path,
    allow: bool,
    mut save: impl FnMut(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<PathBuf> {
    match save(path) {
        Ok(()) => Ok(path.to_path_buf()),
        Err(primary) if allow && path == portable && path != fallback => {
            save(fallback).map_err(|fallback_error| {
                anyhow::anyhow!(
                    "Portable save failed: {primary:#}; user save failed: {fallback_error:#}"
                )
            })?;
            Ok(fallback.to_path_buf())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EndpointPolicy;
    use std::fs;
    #[test]
    fn portable_fallback_is_used_by_production_save() {
        let portable = Path::new("portable/config.json");
        let user = Path::new("user/config.json");
        let mut attempts = Vec::new();
        let saved = persist_with_fallback(portable, portable, user, true, |p| {
            attempts.push(p.to_path_buf());
            if p == portable {
                anyhow::bail!("read-only portable directory");
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(saved, user);
        assert_eq!(attempts, [portable, user]);
        assert!(
            persist_with_fallback(portable, portable, user, true, |_| anyhow::bail!(
                "disk full"
            ))
            .is_err()
        );
        let mut attempts = 0;
        assert!(
            persist_with_fallback(portable, portable, user, false, |_| {
                attempts += 1;
                anyhow::bail!("explicit path");
            })
            .is_err()
        );
        assert_eq!(attempts, 1);
    }
    #[test]
    fn missing_false_true_and_null() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("config.json");
        let p = ProductProfile {
            default_full_control: true,
            ..ProductProfile::default()
        };
        for (text, value) in [
            ("{}", true),
            (r#"{"default_full_control":false}"#, false),
            (r#"{"default_full_control":true}"#, true),
        ] {
            fs::write(&file, text).unwrap();
            assert_eq!(
                AgentConfig::load_file_with_profile(Some(&file), &p)
                    .unwrap()
                    .default_full_control,
                value
            );
        }
        fs::write(&file, r#"{"default_full_control":null}"#).unwrap();
        assert!(AgentConfig::load_file_with_profile(Some(&file), &p).is_err());
    }
    #[test]
    fn config_save_rejects_directories_without_changing_them() {
        let directory = tempfile::tempdir().unwrap();
        let config = AgentConfig::default();
        for path in [std::env::temp_dir(), directory.path().to_path_buf()] {
            assert!(path.is_dir());
            assert!(config.save_file(&path).is_err());
            assert!(path.is_dir());
        }
    }

    #[test]
    fn fixed_endpoint_and_failed_save() {
        let p = ProductProfile {
            endpoint: EndpointPolicy::Fixed {
                relay: "relay.example.invalid:7443".into(),
                server_name: "relay.example.invalid".into(),
            },
            ..ProductProfile::default()
        };
        let c = AgentConfig::for_product(&p);
        let root = tempfile::tempdir().unwrap();
        assert!(c.save_for_product(&p, root.path(), false).is_err());
        let mut bad = c;
        bad.relay = "other.invalid:7443".into();
        assert!(bad.resolve_product(&p, None).is_err());
    }
    #[test]
    fn endpoint_change_clears_trust_and_identity() {
        let p = ProductProfile {
            legacy_remoteops_paths: false,
            ..ProductProfile::default()
        };
        let mut c = AgentConfig::for_product(&p);
        c.relay = "a.invalid:7443".into();
        c.server_name = "a.invalid".into();
        let c = c.resolve_product(&p, None).unwrap();
        let old = c.state_file.clone();
        let mut c = c;
        c.relay = "b.invalid:7443".into();
        c.server_name = "b.invalid".into();
        c.ca_cert = Some("old.pem".into());
        c.tls_fingerprint = Some("old".into());
        let c = c
            .resolve_product(&p, Some(("a.invalid:7443", "a.invalid")))
            .unwrap();
        assert!(c.ca_cert.is_none() && c.tls_fingerprint.is_none());
        assert_ne!(c.state_file, old);
    }
}
