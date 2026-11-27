//! Relay 发布的只读版本目录。产品标识区分公版、定制版和宿主产品。
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// 同一 Relay 向已连接组件公告的已发布版本。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseCatalog {
    /// 目录格式版本，未知版本不参与比对。
    pub schema_version: u16,
    /// 各产品及其平台对应的正式发布记录。
    pub releases: Vec<ReleaseEntry>,
}

/// 某一产品和目标平台唯一的推荐版本。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseEntry {
    /// 稳定产品标识。
    pub product: String,
    /// 编译目标操作系统，如 windows。
    pub os: String,
    /// 编译目标架构，如 x86_64。
    pub arch: String,
    /// 已发布的 SemVer 版本。
    pub version: String,
}

impl ReleaseCatalog {
    /// 校验目录容量、版本和选择条件唯一性。
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1 || self.releases.len() > 256 {
            return Err("版本目录格式或条目数量无效");
        }
        let mut keys = HashSet::new();
        for e in &self.releases {
            if [&e.product, &e.os, &e.arch].iter().any(|s| {
                s.is_empty()
                    || s.len() > 96
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            }) || e.version.len() > 96
                || Version::parse(&e.version).is_err()
            {
                return Err("产品标识、平台或版本格式无效");
            }
            if !keys.insert((&e.product, &e.os, &e.arch)) {
                return Err("版本目录含重复产品和平台");
            }
        }
        Ok(())
    }

    /// 返回已发布版本及其与本地版本的关系。无法识别的平台或版本返回空。
    #[must_use]
    pub fn compare(
        &self,
        product: &str,
        os: &str,
        arch: &str,
        local: &str,
    ) -> Option<(&str, &'static str)> {
        if self.schema_version != 1 {
            return None;
        }
        let entry = self
            .releases
            .iter()
            .find(|e| e.product == product && e.os == os && e.arch == arch)?;
        let status = match Version::parse(local)
            .ok()?
            .cmp_precedence(&Version::parse(&entry.version).ok()?)
        {
            std::cmp::Ordering::Less => "update_available",
            std::cmp::Ordering::Equal => "current",
            std::cmp::Ordering::Greater => "ahead",
        };
        Some((&entry.version, status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compares_numeric_prereleases_only_for_the_matching_build() {
        let mut c = ReleaseCatalog {
            schema_version: 1,
            releases: vec![ReleaseEntry {
                product: "custom".into(),
                os: "windows".into(),
                arch: "x86_64".into(),
                version: "1.0.0-preview.10".into(),
            }],
        };
        assert!(c.validate().is_ok());
        for (local, status) in [
            ("1.0.0-preview.9", "update_available"),
            ("1.0.0-preview.10+abc", "current"),
            ("1.0.0", "ahead"),
        ] {
            assert_eq!(
                c.compare("custom", "windows", "x86_64", local).unwrap().1,
                status
            );
        }
        assert!(c.compare("public", "windows", "x86_64", "1.0.0").is_none());
        assert!(c.compare("custom", "macos", "x86_64", "1.0.0").is_none());
        c.releases.push(c.releases[0].clone());
        assert!(c.validate().is_err());
    }
}
