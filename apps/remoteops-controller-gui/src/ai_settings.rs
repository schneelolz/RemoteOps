//! AI 服务的非敏感配置、Codex 导入和系统凭据存取。

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// Windows 凭据管理器中使用的凭据目标名称。
#[allow(dead_code)]
pub const AI_CREDENTIAL_TARGET: &str = "RemoteOps/AI/BearerToken";

#[cfg(all(target_os = "windows", not(test)))]
const AI_CREDENTIAL_SERVICE: &str = "RemoteOps/AI";
#[cfg(all(target_os = "windows", not(test)))]
const AI_CREDENTIAL_USER: &str = "BearerToken";
const SETTINGS_FILE_NAME: &str = "ai-settings.json";

/// `RemoteOps` 支持的 AI 接口协议。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProtocol {
    /// 自动探测服务支持的协议。
    #[default]
    Auto,
    /// `OpenAI` Responses API。
    Responses,
    /// `OpenAI` Chat Completions API。
    ChatCompletions,
}
impl AiProtocol {
    /// 将 Codex 的 `wire_api` 值转换为 `RemoteOps` 协议。
    fn from_codex_wire_api(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("responses") => Self::Responses,
            Some("chat" | "chat_completions" | "chat-completions") => Self::ChatCompletions,
            _ => Self::Auto,
        }
    }
}

/// 可以安全写入普通配置文件的 AI 设置。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AiSettings {
    /// AI 网关基础地址，不包含具体接口路径。
    pub base_url: String,
    /// 调用的模型名称。
    pub model: String,
    /// 调用协议。
    #[serde(default)]
    pub protocol: AiProtocol,
}

impl AiSettings {
    /// 校验并规范化用户输入。
    pub fn normalized(&self) -> Result<Self> {
        let base_url = self.base_url.trim().trim_end_matches('/').to_owned();
        let model = self.model.trim().to_owned();

        if base_url.is_empty() {
            bail!("AI 服务地址不能为空");
        }
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            bail!("AI 服务地址必须以 http:// 或 https:// 开头");
        }
        if model.is_empty() {
            bail!("AI 模型名称不能为空");
        }

        Ok(Self {
            base_url,
            model,
            protocol: self.protocol,
        })
    }

    /// 从默认应用配置路径读取设置；文件不存在时返回 `None`。
    pub fn load() -> Result<Option<Self>> {
        Self::load_from(&settings_path()?)
    }

    /// 从指定路径读取设置；主要供测试和迁移工具使用。
    pub fn load_from(path: &Path) -> Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("无法读取 AI 配置：{}", path.display()));
            }
        };
        let settings = serde_json::from_str::<Self>(&text)
            .with_context(|| format!("AI 配置格式无效：{}", path.display()))?;
        Ok(Some(settings.normalized()?))
    }

    /// 将设置保存到默认路径，Bearer Token 不会写入该文件。
    pub fn save(&self) -> Result<()> {
        self.save_to(&settings_path()?)
    }

    /// 将设置保存到指定路径，Bearer Token 不会写入该文件。
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let settings = self.normalized()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建 AI 配置目录：{}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(&settings).context("无法序列化 AI 配置")?;
        fs::write(path, text).with_context(|| format!("无法保存 AI 配置：{}", path.display()))
    }

    /// 删除默认路径中的非敏感设置；文件不存在时视为成功。
    pub fn delete() -> Result<()> {
        delete_file_if_exists(&settings_path()?)
    }
}

/// 从 Codex 配置导入的 AI 设置及短期驻留内存的 Token。
///
/// 该类型刻意不实现 `Serialize`，自定义调试输出也不会包含 Token。
#[derive(Clone, Eq, PartialEq)]
pub struct ImportedAiSettings {
    /// 可安全持久化的 AI 设置。
    pub settings: AiSettings,
    /// 需要转存到系统凭据管理器的 Bearer Token。
    pub bearer_token: Option<String>,
    /// 实际读取的 Codex 配置文件。
    pub source: PathBuf,
}

impl std::fmt::Debug for ImportedAiSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImportedAiSettings")
            .field("settings", &self.settings)
            .field("has_bearer_token", &self.bearer_token.is_some())
            .field("source", &self.source)
            .finish()
    }
}

/// 管理 `RemoteOps` AI 设置与 Bearer Token 的统一入口。
#[derive(Clone, Copy, Debug, Default)]
pub struct AiSettingsStore;

impl AiSettingsStore {
    /// 从默认应用配置路径读取非敏感设置。
    pub fn load() -> std::result::Result<Option<AiSettings>, String> {
        AiSettings::load().map_err(format_error)
    }

    /// 保存非敏感设置，并把 Token 写入系统凭据管理器。
    pub fn save(settings: &AiSettings, bearer_token: &str) -> std::result::Result<(), String> {
        if bearer_token.trim().is_empty() {
            return Err("AI Bearer Token 不能为空".to_owned());
        }
        settings.save().map_err(format_error)?;
        save_bearer_token(bearer_token).map_err(format_error)
    }

    /// 从系统凭据管理器读取 Bearer Token。
    pub fn read_token() -> std::result::Result<Option<String>, String> {
        load_bearer_token().map_err(format_error)
    }

    /// 从 Codex 配置导入可保存设置及 Bearer Token。
    pub fn import_from_codex() -> std::result::Result<ImportedAiSettings, String> {
        import_from_codex().map_err(format_error)
    }
}

/// 从首个可用的 Codex 配置文件导入模型、地址、协议和 Token。
pub fn import_from_codex() -> Result<ImportedAiSettings> {
    let candidates = codex_config_candidates();
    let source = candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| anyhow!("未找到 Codex 配置文件"))?;
    import_from_codex_path(&source)
}

/// 从指定 Codex 配置文件导入；主要供测试和显式迁移使用。
pub fn import_from_codex_path(path: &Path) -> Result<ImportedAiSettings> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("无法读取 Codex 配置：{}", path.display()))?;
    // TOML 解析错误可能附带原始行内容，因此只返回不含源码片段的固定错误。
    let config = toml::from_str::<CodexConfig>(&text)
        .map_err(|_| anyhow!("Codex 配置格式无效：{}", path.display()))?;
    let provider_name = config
        .model_provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Codex 配置未指定 model_provider"))?;
    let provider = config
        .model_providers
        .get(provider_name)
        .ok_or_else(|| anyhow!("Codex 配置中不存在 model_providers.{provider_name}"))?;

    let settings = AiSettings {
        base_url: provider.base_url.clone().unwrap_or_default(),
        model: config.model.unwrap_or_default(),
        protocol: AiProtocol::from_codex_wire_api(provider.wire_api.as_deref()),
    }
    .normalized()?;
    let bearer_token = provider
        .experimental_bearer_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    Ok(ImportedAiSettings {
        settings,
        bearer_token,
        source: path.to_path_buf(),
    })
}

/// 从系统凭据管理器读取 `RemoteOps` AI Bearer Token。
pub fn load_bearer_token() -> Result<Option<String>> {
    credential_store::load()
}

/// 将 `RemoteOps` AI Bearer Token 保存到系统凭据管理器。
pub fn save_bearer_token(token: &str) -> Result<()> {
    let token = token.trim();
    if token.is_empty() {
        bail!("AI Bearer Token 不能为空");
    }
    credential_store::save(token)
}

/// 删除系统凭据管理器中的 `RemoteOps` AI Bearer Token。
pub fn delete_bearer_token() -> Result<()> {
    credential_store::delete()
}

/// 同时删除非敏感配置和系统凭据中的 Token。
pub fn delete_all() -> Result<()> {
    AiSettings::delete()?;
    delete_bearer_token()
}

/// 返回 `RemoteOps` 非敏感 AI 配置的默认路径。
pub fn settings_path() -> Result<PathBuf> {
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local_app_data)
            .join("RemoteOps")
            .join(SETTINGS_FILE_NAME));
    }
    if let Some(user_profile) = env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(user_profile)
            .join("AppData")
            .join("Local")
            .join("RemoteOps")
            .join(SETTINGS_FILE_NAME));
    }
    bail!("无法确定用户配置目录")
}

fn codex_config_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(codex_home) = env::var_os("CODEX_HOME") {
        push_unique(&mut paths, PathBuf::from(codex_home).join("config.toml"));
    }
    if let Some(user_profile) = env::var_os("USERPROFILE") {
        push_unique(
            &mut paths,
            PathBuf::from(user_profile)
                .join(".codex")
                .join("config.toml"),
        );
    }
    paths
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

fn delete_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("无法删除 AI 配置：{}", path.display())),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn format_error(error: anyhow::Error) -> String {
    format!("{error:#}")
}

#[derive(Deserialize)]
struct CodexConfig {
    model: Option<String>,
    model_provider: Option<String>,
    #[serde(default)]
    model_providers: BTreeMap<String, CodexProvider>,
}

#[derive(Deserialize)]
struct CodexProvider {
    base_url: Option<String>,
    wire_api: Option<String>,
    experimental_bearer_token: Option<String>,
}

#[cfg(all(target_os = "windows", not(test)))]
mod credential_store {
    use std::collections::HashMap;

    use anyhow::{Context, Result};
    use keyring::{Entry, Error};

    use super::{AI_CREDENTIAL_SERVICE, AI_CREDENTIAL_TARGET, AI_CREDENTIAL_USER};

    pub(super) fn load() -> Result<Option<String>> {
        let entry = entry()?;
        match entry.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(Error::NoEntry) => Ok(None),
            Err(error) => Err(error).context("无法从 Windows 凭据管理器读取 AI Token"),
        }
    }

    pub(super) fn save(token: &str) -> Result<()> {
        entry()?
            .set_password(token)
            .context("无法将 AI Token 保存到 Windows 凭据管理器")
    }

    pub(super) fn delete() -> Result<()> {
        let entry = entry()?;
        match entry.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(error) => Err(error).context("无法从 Windows 凭据管理器删除 AI Token"),
        }
    }

    fn entry() -> Result<Entry> {
        Entry::store_status()
            .as_ref()
            .map_err(|error| anyhow::anyhow!("无法初始化 Windows 凭据管理器：{error}"))?;
        let modifiers = HashMap::from([("target", AI_CREDENTIAL_TARGET)]);
        let inner = keyring_core::Entry::new_with_modifiers(
            AI_CREDENTIAL_SERVICE,
            AI_CREDENTIAL_USER,
            &modifiers,
        )
        .context("无法打开 Windows 凭据管理器")?;
        Ok(Entry { inner })
    }
}

#[cfg(any(not(target_os = "windows"), test))]
mod credential_store {
    use anyhow::{Result, anyhow};
    use std::sync::{Mutex, OnceLock};

    // 非 Windows 和自动化测试使用进程内存，避免把测试 Token 写入真实凭据库。
    static TOKEN: OnceLock<Mutex<Option<String>>> = OnceLock::new();

    pub(super) fn load() -> Result<Option<String>> {
        token()
            .lock()
            .map(|value| value.clone())
            .map_err(lock_error)
    }

    pub(super) fn save(value: &str) -> Result<()> {
        *token().lock().map_err(lock_error)? = Some(value.to_owned());
        Ok(())
    }

    pub(super) fn delete() -> Result<()> {
        *token().lock().map_err(lock_error)? = None;
        Ok(())
    }

    fn token() -> &'static Mutex<Option<String>> {
        TOKEN.get_or_init(|| Mutex::new(None))
    }

    fn lock_error<T>(_: std::sync::PoisonError<T>) -> anyhow::Error {
        anyhow!("AI Token 内存存储已损坏")
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::{Mutex, OnceLock},
    };

    use super::{
        AiProtocol, AiSettings, delete_bearer_token, import_from_codex_path, load_bearer_token,
        save_bearer_token,
    };

    #[test]
    fn settings_round_trip_does_not_contain_token() {
        let _guard = test_lock().lock().expect("test lock should work");
        let directory = temporary_directory("settings");
        let path = directory.join("ai-settings.json");
        let settings = AiSettings {
            base_url: "https://gateway.example/v1/".to_owned(),
            model: " test-model ".to_owned(),
            protocol: AiProtocol::Responses,
        };

        settings.save_to(&path).expect("settings should save");
        let saved_text = fs::read_to_string(&path).expect("settings should be readable");
        let loaded = AiSettings::load_from(&path)
            .expect("settings should load")
            .expect("settings should exist");

        assert_eq!(loaded.base_url, "https://gateway.example/v1");
        assert_eq!(loaded.model, "test-model");
        assert_eq!(loaded.protocol, AiProtocol::Responses);
        assert!(!saved_text.to_ascii_lowercase().contains("token"));
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn imports_selected_codex_provider_without_exposing_token_in_debug() {
        let _guard = test_lock().lock().expect("test lock should work");
        let directory = temporary_directory("codex-import");
        let path = directory.join("config.toml");
        fs::write(
            &path,
            r#"
model_provider = "private_gateway"
model = "gpt-test"

[model_providers.private_gateway]
base_url = "http://127.0.0.1:3001/"
wire_api = "responses"
experimental_bearer_token = "test-secret-never-log"

[model_providers.unused]
base_url = "https://unused.example"
wire_api = "chat_completions"
"#,
        )
        .expect("Codex fixture should save");

        let imported = import_from_codex_path(&path).expect("Codex settings should import");

        assert_eq!(imported.settings.base_url, "http://127.0.0.1:3001");
        assert_eq!(imported.settings.model, "gpt-test");
        assert_eq!(imported.settings.protocol, AiProtocol::Responses);
        assert_eq!(
            imported.bearer_token.as_deref(),
            Some("test-secret-never-log")
        );
        assert!(!format!("{imported:?}").contains("test-secret-never-log"));
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn malformed_codex_config_error_does_not_echo_secret_source() {
        let _guard = test_lock().lock().expect("test lock should work");
        let directory = temporary_directory("invalid-codex");
        let path = directory.join("config.toml");
        fs::write(
            &path,
            r#"
model_provider = "private_gateway"
model = "gpt-test"
[model_providers.private_gateway]
base_url = "https://gateway.example"
experimental_bearer_token = "secret-that-must-not-appear
"#,
        )
        .expect("invalid Codex fixture should save");

        let error = import_from_codex_path(&path).expect_err("invalid TOML should fail");

        assert!(!format!("{error:#}").contains("secret-that-must-not-appear"));
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn credential_store_supports_save_read_and_delete() {
        let _guard = test_lock().lock().expect("test lock should work");
        delete_bearer_token().expect("credential cleanup should work");

        save_bearer_token("test-bearer-token").expect("credential should save");
        assert_eq!(
            load_bearer_token()
                .expect("credential should load")
                .as_deref(),
            Some("test-bearer-token")
        );

        delete_bearer_token().expect("credential should delete");
        assert_eq!(
            load_bearer_token().expect("credential lookup should work"),
            None
        );
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "remoteops-ai-settings-{label}-{}-{}",
            std::process::id(),
            current_test_nonce()
        ));
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }

    fn current_test_nonce() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after Unix epoch")
            .as_nanos()
    }

    fn test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
}
