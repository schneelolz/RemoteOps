//! `RemoteOps` 共享国际化资源加载器。

use std::{collections::BTreeMap, env, fmt, fs, path::Path, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 支持的界面语言。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Language {
    /// 简体中文。
    #[default]
    ZhCn,
    /// 美国英语。
    EnUs,
}

impl Language {
    /// 从 BCP-47 或常见系统语言字符串解析语言。
    #[must_use]
    pub fn parse(value: &str) -> Self {
        let value = value.trim().to_ascii_lowercase();
        if value.starts_with("en") {
            Self::EnUs
        } else {
            Self::ZhCn
        }
    }

    /// 按环境变量和操作系统区域设置推断首选语言。
    #[must_use]
    pub fn detect() -> Self {
        env::var("REMOTEOPS_LANG")
            .or_else(|_| env::var("LANGUAGE"))
            .or_else(|_| env::var("LANG"))
            .ok()
            .or_else(sys_locale::get_locale)
            .map_or(Self::ZhCn, |value| Self::parse(&value))
    }

    /// 返回稳定语言标识。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl FromStr for Language {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim().to_ascii_lowercase();
        if normalized.starts_with("zh") {
            Ok(Self::ZhCn)
        } else if normalized.starts_with("en") {
            Ok(Self::EnUs)
        } else {
            Err(format!("不支持的语言：{value}，可选值为 zh-CN 或 en-US"))
        }
    }
}

/// 语言包加载错误。
#[derive(Debug, Error)]
pub enum I18nError {
    /// 文件读取失败。
    #[error("无法读取语言包：{0}")]
    Io(#[from] std::io::Error),
    /// JSON 格式无效。
    #[error("语言包 JSON 无效：{0}")]
    Json(#[from] serde_json::Error),
    /// 语言包包含空键或空值。
    #[error("语言包包含空键或空文案")]
    InvalidEntry,
}

/// 只包含文本和占位符的安全语言包。
#[derive(Clone, Debug)]
pub struct Translator {
    language: Language,
    fallback: Language,
    entries: BTreeMap<String, String>,
    fallback_entries: BTreeMap<String, String>,
}

impl Translator {
    /// 按系统环境创建内置翻译器。
    #[must_use]
    pub fn detect() -> Self {
        Self::new(Language::detect())
    }

    /// 创建指定语言的内置翻译器。
    #[must_use]
    pub fn new(language: Language) -> Self {
        let entries = builtin_entries(language);
        let fallback_entries = builtin_entries(Language::ZhCn);
        Self {
            language,
            fallback: Language::ZhCn,
            entries,
            fallback_entries,
        }
    }

    /// 切换当前语言并重新加载内置文案。
    pub fn set_language(&mut self, language: Language) {
        self.language = language;
        self.entries = builtin_entries(language);
    }

    /// 返回当前语言。
    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }

    /// 从外部 JSON 语言包覆盖当前语言；失败时由调用方保留旧翻译器。
    ///
    /// # Errors
    ///
    /// 当文件无法读取、JSON 无效或包含空键值时返回错误。
    pub fn overlay_file(&mut self, path: impl AsRef<Path>) -> Result<(), I18nError> {
        let text = fs::read_to_string(path)?;
        let entries: BTreeMap<String, String> = serde_json::from_str(&text)?;
        for (key, value) in &entries {
            if key.trim().is_empty() || value.trim().is_empty() {
                return Err(I18nError::InvalidEntry);
            }
        }
        self.entries.extend(entries);
        Ok(())
    }

    /// 获取文案；当前语言缺失时回退到中文，再缺失时返回键名。
    #[must_use]
    pub fn text(&self, key: &str) -> String {
        self.entries
            .get(key)
            .or_else(|| self.fallback_entries.get(key))
            .cloned()
            .unwrap_or_else(|| key.to_owned())
    }

    /// 获取文案并替换简单的 `{name}` 占位符。
    #[must_use]
    pub fn text_with(&self, key: &str, replacements: &[(&str, &str)]) -> String {
        let mut value = self.text(key);
        for (name, replacement) in replacements {
            value = value.replace(&format!("{{{name}}}"), replacement);
        }
        value
    }

    /// 返回回退语言。
    #[must_use]
    pub const fn fallback_language(&self) -> Language {
        self.fallback
    }
}

#[derive(Deserialize)]
struct EmbeddedLanguageFile(BTreeMap<String, String>);

fn builtin_entries(language: Language) -> BTreeMap<String, String> {
    let text = match language {
        Language::ZhCn => include_str!("../resources/zh-CN.json"),
        Language::EnUs => include_str!("../resources/en-US.json"),
    };
    serde_json::from_str::<EmbeddedLanguageFile>(text)
        .map(|file| file.0)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_falls_back_safely() {
        assert_eq!(Language::parse("en-US"), Language::EnUs);
        assert_eq!(Language::parse("zh-CN"), Language::ZhCn);
        let translator = Translator::new(Language::EnUs);
        assert_eq!(
            translator.text("app.agent_title"),
            "RemoteOps Remote Assistance"
        );
        assert_eq!(translator.text("missing.key"), "missing.key");
    }

    #[test]
    fn embedded_languages_expose_the_same_keys() {
        let zh = builtin_entries(Language::ZhCn);
        let en = builtin_entries(Language::EnUs);
        assert_eq!(
            zh.keys().collect::<Vec<_>>(),
            en.keys().collect::<Vec<_>>(),
            "内置中英文语言包必须保持相同键集合"
        );
    }

    #[test]
    fn replaces_only_text_placeholders() {
        let translator = Translator::new(Language::ZhCn);
        assert_eq!(
            translator.text_with("controller.session_id", &[("id", "session-1")]),
            "会话 ID：session-1"
        );
    }
}

