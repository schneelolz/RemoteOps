//! Parser-backed, lossless editing of the single `RemoteOps` Codex MCP entry.

use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::{ErrorKind, Write},
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, bail};
use toml_edit::{Array, Document, DocumentMut, Item, Table, value};
use uuid::Uuid;

/// Check syntax and supported table shapes without creating or changing any files.
pub(super) fn validate(path: &Path) -> anyhow::Result<()> {
    let original = read_config(path)?;
    render(
        original.as_deref().unwrap_or_default(),
        "remoteops-controller-mcp",
        "remoteops-mcp.json",
        "agent-controlled",
        false,
    )?;
    Ok(())
}

/// Replace only `mcp_servers.remoteops`, retaining all unrelated source bytes.
pub(super) fn configure(
    path: &Path,
    command: &str,
    mcp_config: &str,
    command_mode: &str,
    legacy_env: bool,
) -> anyhow::Result<()> {
    update(path, |input| {
        render(input, command, mcp_config, command_mode, legacy_env)
    })
}

/// Remove only the parsed `mcp_servers.remoteops` subtree, if present.
pub(super) fn unconfigure(path: &Path) -> anyhow::Result<()> {
    update(path, remove_remoteops)
}

/// Return only the non-secret fields needed by installation verification.
pub(super) fn inspect(path: &Path) -> anyhow::Result<serde_json::Value> {
    let input = read_config(path)?.context("未找到 Codex 配置文件")?;
    inspect_source(&input)
}

fn inspect_source(input: &str) -> anyhow::Result<serde_json::Value> {
    let document = parse(input)?;
    let remoteops = document
        .get("mcp_servers")
        .and_then(Item::as_table_like)
        .and_then(|servers| servers.get("remoteops"))
        .and_then(Item::as_table_like)
        .context("未找到有效的 mcp_servers.remoteops 配置表")?;
    let control_mode_approval = remoteops
        .get("tools")
        .and_then(Item::as_table_like)
        .and_then(|tools| tools.get("set_control_mode"))
        .and_then(Item::as_table_like)
        .and_then(|tool| tool.get("approval_mode"))
        .and_then(Item::as_str);
    let expected_args = remoteops
        .get("args")
        .and_then(Item::as_array)
        .filter(|args| {
            args.len() == 4
                && args.get(0).and_then(toml_edit::Value::as_str) == Some("--config")
                && args.get(2).and_then(toml_edit::Value::as_str) == Some("--command-mode")
                && args
                    .get(1)
                    .and_then(toml_edit::Value::as_str)
                    .is_some_and(|config| !config.is_empty())
                && args
                    .get(3)
                    .and_then(toml_edit::Value::as_str)
                    .is_some_and(valid_command_mode)
        });
    let config = expected_args
        .and_then(|args| args.get(1))
        .and_then(toml_edit::Value::as_str);
    let mode = expected_args
        .and_then(|args| args.get(3))
        .and_then(toml_edit::Value::as_str);
    let legacy_env = match remoteops.get("env_vars") {
        None => false,
        Some(item)
            if item.as_array().is_some_and(|env| {
                env.len() == 2
                    && env.get(0).and_then(toml_edit::Value::as_str)
                        == Some("REMOTEOPS_CONTROLLER_TOKEN")
                    && env.get(1).and_then(toml_edit::Value::as_str)
                        == Some("REMOTEOPS_CONTROLLER_OWNER_ID")
            }) =>
        {
            true
        }
        Some(_) => bail!("RemoteOps 环境变量转发配置不符合受支持的安装格式"),
    };
    Ok(serde_json::json!({
        "command": remoteops.get("command").and_then(Item::as_str),
        "tool_timeout_sec": remoteops.get("tool_timeout_sec").and_then(Item::as_integer),
        "default_tools_approval_mode": remoteops.get("default_tools_approval_mode").and_then(Item::as_str),
        "set_control_mode_approval_mode": control_mode_approval,
        "legacy_env": legacy_env,
        "args_config": config,
        "command_mode": mode,
    }))
}

fn valid_command_mode(mode: &str) -> bool {
    matches!(
        mode,
        "readonly" | "approval" | "agent-controlled" | "full-access"
    )
}

fn render(
    input: &str,
    command: &str,
    mcp_config: &str,
    command_mode: &str,
    legacy_env: bool,
) -> anyhow::Result<String> {
    if command.is_empty() || mcp_config.is_empty() {
        bail!("MCP 命令和配置路径不能为空");
    }
    if !valid_command_mode(command_mode) {
        bail!("不支持的 MCP 命令模式");
    }
    let mut output = remove_remoteops(input)?;
    // Do not normalize existing line endings, including those inside string values.
    let newline = if input.contains("\r\n") { "\r\n" } else { "\n" };
    if !output.is_empty() && !output.ends_with('\n') {
        output.push_str(newline);
    }
    output.push_str(
        &remoteops_table(command, mcp_config, command_mode, legacy_env).replace('\n', newline),
    );
    parse(&output).context("生成的 Codex 配置未通过校验；原文件未修改")?;
    Ok(output)
}

fn remoteops_table(
    command: &str,
    mcp_config: &str,
    command_mode: &str,
    legacy_env: bool,
) -> String {
    let mut remoteops = Table::new();
    remoteops.decor_mut().set_prefix("");
    remoteops.insert("command", value(command));
    let args: Array = ["--config", mcp_config, "--command-mode", command_mode]
        .into_iter()
        .collect();
    remoteops.insert("args", value(args));
    remoteops.insert("startup_timeout_sec", value(15));
    remoteops.insert("tool_timeout_sec", value(360));
    remoteops.insert("enabled", value(true));
    remoteops.insert("required", value(false));
    remoteops.insert("default_tools_approval_mode", value("approve"));
    if legacy_env {
        let env_vars: Array = [
            "REMOTEOPS_CONTROLLER_TOKEN",
            "REMOTEOPS_CONTROLLER_OWNER_ID",
        ]
        .into_iter()
        .collect();
        remoteops.insert("env_vars", value(env_vars));
    }
    let mut control_mode = Table::new();
    control_mode.decor_mut().set_prefix("");
    control_mode.insert("approval_mode", value("prompt"));
    let mut tools = Table::new();
    tools.set_implicit(true);
    tools.insert("set_control_mode", Item::Table(control_mode));
    remoteops.insert("tools", Item::Table(tools));
    let mut servers = Table::new();
    servers.set_implicit(true);
    servers.insert("remoteops", Item::Table(remoteops));
    let mut document = DocumentMut::new();
    document.insert("mcp_servers", Item::Table(servers));
    document.to_string()
}

fn parse(input: &str) -> anyhow::Result<Document<&str>> {
    // Parser diagnostics can contain an existing token/password. Do not echo them.
    Document::parse(input).map_err(|_| anyhow!("Codex 配置不是有效 TOML；原文件未修改"))
}

fn remove_remoteops(input: &str) -> anyhow::Result<String> {
    let document = parse(input)?;
    let Some(servers) = document.get("mcp_servers") else {
        return Ok(input.to_owned());
    };
    let servers = servers
        .as_table()
        .context("mcp_servers 必须使用普通 TOML 表；原文件未修改")?;
    let Some(remoteops) = servers.get("remoteops") else {
        return Ok(input.to_owned());
    };
    if !remoteops.is_table() && !remoteops.is_inline_table() {
        bail!("mcp_servers.remoteops 必须是 TOML 表；原文件未修改");
    }

    // Table spans identify actual parsed headers, never lookalikes in strings or
    // comments. Value spans also cover multiline arrays, strings and inline tables.
    // Work on original spans rather than serializing the rest of the document:
    // toml_edit's serializer intentionally normalizes statement line endings.
    let mut ranges = Vec::new();
    collect_statements(remoteops, input, &mut ranges)?;
    ranges.sort_unstable_by_key(|range| range.start);
    let mut output = String::with_capacity(input.len());
    let mut copied_until = 0;
    for range in ranges {
        if range.start > copied_until {
            output.push_str(&input[copied_until..range.start]);
        }
        copied_until = copied_until.max(range.end);
    }
    output.push_str(&input[copied_until..]);
    let checked = parse(&output)?;
    if checked
        .get("mcp_servers")
        .and_then(Item::as_table)
        .is_some_and(|table| table.contains_key("remoteops"))
    {
        bail!("无法安全移除现有 RemoteOps 配置；原文件未修改");
    }
    Ok(output)
}

fn collect_statements(
    item: &Item,
    input: &str,
    ranges: &mut Vec<Range<usize>>,
) -> anyhow::Result<()> {
    match item {
        Item::Table(table) => {
            if !table.is_implicit() && !table.is_dotted() {
                ranges.push(statement_range(input, table.span())?);
            }
            for (_, child) in table {
                collect_statements(child, input, ranges)?;
            }
        }
        Item::ArrayOfTables(array) => {
            for table in array {
                ranges.push(statement_range(input, table.span())?);
                for (_, child) in table {
                    collect_statements(child, input, ranges)?;
                }
            }
        }
        Item::Value(value) => ranges.push(statement_range(input, value.span())?),
        Item::None => bail!("现有 RemoteOps 配置包含不支持的节点；原文件未修改"),
    }
    Ok(())
}

fn statement_range(input: &str, span: Option<Range<usize>>) -> anyhow::Result<Range<usize>> {
    let span = span.context("现有 RemoteOps 配置缺少源位置信息；原文件未修改")?;
    let mut start = input[..span.start].rfind('\n').map_or(0, |index| index + 1);
    // The parser tolerates an initial UTF-8 BOM. It belongs to the document, not
    // the first statement, and must survive replacing that statement.
    if start == 0 && input.starts_with('\u{feff}') {
        start = '\u{feff}'.len_utf8();
    }
    let end = input[span.end..]
        .find('\n')
        .map_or(input.len(), |index| span.end + index + 1);
    Ok(start..end)
}

fn read_config(path: &Path) -> anyhow::Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                bail!("Codex 配置必须是普通文件，不能是符号链接或目录");
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("无法检查 Codex 配置"),
    }
    fs::read_to_string(path)
        .map(Some)
        .context("无法读取 UTF-8 Codex 配置；原文件未修改")
}

fn update(path: &Path, transform: impl Fn(&str) -> anyhow::Result<String>) -> anyhow::Result<()> {
    // Validate before even creating a lock, parent directory, temporary or backup.
    let original = read_config(path)?;
    let rendered = transform(original.as_deref().unwrap_or_default())?;
    if original.as_deref().unwrap_or_default() == rendered {
        return Ok(());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).context("无法创建 Codex 配置目录")?;
    let _lock = lock_config(path)?;
    // Another installer may have completed between the preflight and lock.
    let original = read_config(path)?;
    let rendered = transform(original.as_deref().unwrap_or_default())?;
    if original.as_deref().unwrap_or_default() == rendered {
        return Ok(());
    }
    atomic_replace(path, parent, original.as_deref(), &rendered)
}

fn sibling_path(path: &Path, suffix: &str) -> anyhow::Result<PathBuf> {
    let mut filename = path
        .file_name()
        .context("Codex 配置路径缺少文件名")?
        .to_owned();
    filename.push(suffix);
    Ok(path.with_file_name(filename))
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

fn lock_config(path: &Path) -> anyhow::Result<File> {
    let lock_path = sibling_path(path, ".remoteops.lock")?;
    if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        bail!("Codex 配置锁必须是普通文件");
    }
    let lock = private_options()
        .read(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .context("无法打开 Codex 配置锁")?;
    lock.try_lock()
        .context("另一个安装正在修改 Codex 配置，请稍后重试")?;
    // Leave the lock file in place: unlinking it would permit parallel lock inodes.
    Ok(lock)
}

fn write_synced(path: &Path, bytes: &[u8], permissions: Option<Permissions>) -> anyhow::Result<()> {
    let mut file = private_options().create_new(true).open(path)?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions)?;
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn atomic_replace(
    path: &Path,
    parent: &Path,
    original: Option<&str>,
    rendered: &str,
) -> anyhow::Result<()> {
    let id = Uuid::new_v4();
    let temporary = sibling_path(path, &format!(".remoteops-{id}.tmp"))?;
    let permissions = if original.is_some() {
        Some(fs::metadata(path)?.permissions())
    } else {
        None
    };
    let result = (|| {
        write_synced(&temporary, rendered.as_bytes(), permissions)?;
        if read_config(path)?.as_deref() != original {
            bail!("Codex 配置在安装期间发生变化，请重试；原文件未修改");
        }
        if let Some(original) = original {
            // A unique, recognizable sibling backup is never overwritten. It may
            // contain unrelated private configuration, so use restrictive creation.
            let backup = sibling_path(path, &format!(".remoteops-{id}.bak"))?;
            write_synced(&backup, original.as_bytes(), None).context("无法备份 Codex 配置")?;
            #[cfg(unix)]
            File::open(parent)?.sync_all()?;
        }
        if read_config(path)?.as_deref() != original {
            bail!("Codex 配置在备份期间发生变化，请重试；原文件未修改");
        }
        fs::rename(&temporary, path).context("无法原子替换 Codex 配置；原文件未修改")?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        #[cfg(not(unix))]
        let _ = parent;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(input: &str) -> String {
        render(
            input,
            "/Applications/RemoteOps/mcp",
            "/tmp/mcp.json",
            "agent-controlled",
            false,
        )
        .unwrap()
    }

    fn temp_directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!("remoteops-codex-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn replaces_all_quoted_header_variants() {
        for header in [
            "[mcp_servers.remoteops]",
            "[mcp_servers.\"remoteops\"]",
            "[\"mcp_servers\".remoteops]",
            "[\"mcp_servers\".\"remoteops\"]",
            "[ 'mcp_servers' . 'remoteops' ]",
            "[mcp_servers.\"remote\\u006fps\"]",
        ] {
            let original = format!(
                "# root\nmodel = 'test'\n{header}\ncommand = 'old'\n[mcp_servers.other]\ncommand = 'keep'\n"
            );
            let rendered = configured(&original);
            assert!(
                rendered
                    .starts_with("# root\nmodel = 'test'\n[mcp_servers.other]\ncommand = 'keep'\n")
            );
            assert!(!rendered.contains("command = 'old'"));
            assert_eq!(configured(&rendered), rendered);
        }
    }

    #[test]
    fn preserves_unrelated_multiline_strings_comments_and_mixed_newlines_exactly() {
        let before = "# keep root\r\nmodel = 'test' # exact\ntext = \"\"\"\r\n[mcp_servers.remoteops]\ncommand = 'inside a string'\r\n\"\"\"\nother = '''\r\n[\"mcp_servers\".\"remoteops\"]\r\n'''\n";
        let after = "# keep other\r\n[mcp_servers.other]\ncommand = 'unchanged' # exact\r\nargs = [\r\n  'one', # first\n  'two',\r\n]\n[profiles.work]\r\napproval_policy = 'on-request'\n";
        let original = format!("{before}[mcp_servers.remoteops]\r\ncommand = 'old'\n{after}");
        let rendered = configured(&original);
        assert!(rendered.starts_with(&format!("{before}{after}")));
        assert_eq!(configured(&rendered), rendered);
    }

    #[test]
    fn replaces_only_target_subtree_with_interleaved_children() {
        let original = "approval_policy = 'on-request'\n[mcp_servers.remoteops]\ncommand = 'old'\n[mcp_servers.other]\ncommand = 'keep'\n[mcp_servers.remoteops.env]\nOLD = 'remove'\n[mcp_servers.remoteops.tools.set_control_mode]\napproval_mode = 'approve'\n[[mcp_servers.remoteops.legacy]]\nname = 'remove'\n[mcp_servers.\"remoteops.extra\"]\ncommand = 'literal dot'\n[remoteops]\ncommand = 'unrelated root'\n";
        let rendered = configured(original);
        assert!(rendered.starts_with("approval_policy = 'on-request'\n[mcp_servers.other]\ncommand = 'keep'\n[mcp_servers.\"remoteops.extra\"]\ncommand = 'literal dot'\n[remoteops]\ncommand = 'unrelated root'\n"));
        assert!(!rendered.contains("remove"));
        let document = parse(&rendered).unwrap();
        let remoteops = &document["mcp_servers"]["remoteops"];
        assert_eq!(
            remoteops["default_tools_approval_mode"].as_str(),
            Some("approve")
        );
        assert_eq!(
            remoteops["tools"]["set_control_mode"]["approval_mode"].as_str(),
            Some("prompt")
        );
        assert_eq!(remoteops["startup_timeout_sec"].as_integer(), Some(15));
        assert_eq!(remoteops["tool_timeout_sec"].as_integer(), Some(360));
        assert_eq!(remoteops["enabled"].as_bool(), Some(true));
        assert_eq!(remoteops["required"].as_bool(), Some(false));
    }

    #[test]
    fn supports_dotted_and_inline_remoteops_entries() {
        for original in [
            "mcp_servers.remoteops.command = 'old'\nmcp_servers.other.command = 'keep'\nmcp_servers.remoteops.args = ['old']\n",
            "[mcp_servers]\nremoteops = { command = 'old', args = ['old'] }\nother = { command = 'keep' }\n",
            "[mcp_servers]\nremoteops.command = 'old'\nother.command = 'keep'\n",
            "mcp_servers.remoteops = { command = 'old' }\nmcp_servers.other = { command = 'keep' }\n",
        ] {
            let rendered = configured(original);
            assert!(!rendered.contains("'old'"));
            assert!(rendered.contains("'keep'"));
            assert_eq!(configured(&rendered), rendered);
        }
    }

    #[test]
    fn quotes_paths_and_replaces_legacy_environment_setting() {
        let command = "C:\\Program Files\\RemoteOps\\mcp.exe";
        let config = "C:\\Users\\demo\\a \"quote\"\\mcp.json";
        let rendered = render("", command, config, "readonly", true).unwrap();
        let document = parse(&rendered).unwrap();
        let remoteops = &document["mcp_servers"]["remoteops"];
        assert_eq!(remoteops["command"].as_str(), Some(command));
        let args = remoteops["args"].as_array().unwrap();
        assert_eq!(args.get(1).and_then(toml_edit::Value::as_str), Some(config));
        assert_eq!(
            args.get(3).and_then(toml_edit::Value::as_str),
            Some("readonly")
        );
        let env = remoteops["env_vars"].as_array().unwrap();
        assert_eq!(env.len(), 2);
        assert_eq!(
            env.get(0).and_then(toml_edit::Value::as_str),
            Some("REMOTEOPS_CONTROLLER_TOKEN")
        );
        let rendered = render(&rendered, command, config, "readonly", false).unwrap();
        assert!(!rendered.contains("env_vars"));
    }

    #[test]
    fn invalid_or_conflicting_input_has_no_filesystem_side_effects() {
        for original in [
            "model = 'unfinished",
            "[mcp_servers.remoteops]\ncommand = 'a'\n[\"mcp_servers\".\"remoteops\"]\ncommand = 'b'\n",
            "mcp_servers = 'conflict'\n",
            "mcp_servers = { other = { command = 'keep' } }\n",
            "[[mcp_servers]]\ncommand = 'conflict'\n",
            "[mcp_servers]\nremoteops = false\n",
            "[[mcp_servers.remoteops]]\ncommand = 'conflict'\n",
        ] {
            let directory = temp_directory();
            let path = directory.join("config.toml");
            fs::write(&path, original).unwrap();
            assert!(validate(&path).is_err());
            assert!(configure(&path, "mcp", "mcp.json", "agent-controlled", false).is_err());
            assert!(unconfigure(&path).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn creates_missing_file_and_idempotently_backs_up_only_changed_files() {
        let directory = temp_directory();
        let path = directory.join("nested/config.toml");
        validate(&path).unwrap();
        unconfigure(&path).unwrap();
        assert!(!path.parent().unwrap().exists());
        configure(&path, "mcp", "mcp.json", "agent-controlled", false).unwrap();
        let first = fs::read_to_string(&path).unwrap();
        let count = fs::read_dir(path.parent().unwrap()).unwrap().count();
        configure(&path, "mcp", "mcp.json", "agent-controlled", false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), first);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), count);
        configure(&path, "mcp2", "mcp.json", "agent-controlled", false).unwrap();
        let backups: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "bak")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read_to_string(backups[0].path()).unwrap(), first);
        unconfigure(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        assert_eq!(fs::read_to_string(backups[0].path()).unwrap(), first);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unconfigure_preserves_exact_unrelated_bytes() {
        let before = "# keep\r\nmodel = 'test'\n";
        let after = "# trailing\r\n[mcp_servers.other]\r\ncommand = 'keep'\n";
        let original =
            format!("{before}[\"mcp_servers\".\"remoteops\"]\r\ncommand = 'old'\r\n{after}");
        assert_eq!(
            remove_remoteops(&original).unwrap(),
            format!("{before}{after}")
        );
    }

    #[test]
    fn preserves_initial_bom_and_missing_final_newline() {
        let original = "\u{feff}[mcp_servers.remoteops]\r\ncommand = 'old'\r\n[mcp_servers.other]\ncommand = 'keep'";
        let untouched = "\u{feff}[mcp_servers.other]\ncommand = 'keep'";
        assert_eq!(remove_remoteops(original).unwrap(), untouched);
        let rendered = configured(original);
        assert!(rendered.starts_with(untouched));
        assert_eq!(configured(&rendered), rendered);
    }

    #[test]
    fn parser_errors_do_not_echo_private_values() {
        let error = render(
            "token = 'do-not-print-me",
            "mcp",
            "config",
            "readonly",
            false,
        )
        .unwrap_err();
        assert!(!format!("{error:#}").contains("do-not-print-me"));
    }

    #[test]
    fn inspector_reads_actual_quoted_table_and_ignores_multiline_lookalikes() {
        let source = "[mcp_servers.other]\ntext = '''\n[mcp_servers.remoteops]\ncommand = '/do-not-run'\nargs = ['--config', '/not-the-config', '--command-mode', 'full-access']\n'''\n[\"mcp_servers\".\"remoteops\"]\ncommand = '/installed/mcp'\nargs = ['--config', '/installed/config.json', '--command-mode', 'agent-controlled']\ntool_timeout_sec = 360\ndefault_tools_approval_mode = 'approve'\n[\"mcp_servers\".\"remoteops\".tools.set_control_mode]\napproval_mode = 'prompt'\n";
        let inspected = inspect_source(source).unwrap();
        assert_eq!(inspected["command"], "/installed/mcp");
        assert_eq!(inspected["args_config"], "/installed/config.json");
        assert_eq!(inspected["command_mode"], "agent-controlled");
        assert_eq!(inspected["tool_timeout_sec"], 360);
        assert_eq!(inspected["default_tools_approval_mode"], "approve");
        assert_eq!(inspected["set_control_mode_approval_mode"], "prompt");
        assert_eq!(inspected["legacy_env"], false);
        assert!(!inspected.to_string().contains("do-not-run"));
    }

    #[test]
    fn inspector_never_emits_environment_or_nonstandard_arguments() {
        for args in [
            "['--config', '/private', '--command-mode', 'unrecognized']",
            "['--token', 'do-not-print-me']",
            "['--config', '/private', '--command-mode', 'readonly', 'do-not-print-me']",
            "['--config', 42, '--command-mode', 'readonly']",
        ] {
            let source = format!(
                "[mcp_servers.remoteops]\ncommand = 'mcp'\nargs = {args}\nenv_vars = ['REMOTEOPS_CONTROLLER_TOKEN', 'REMOTEOPS_CONTROLLER_OWNER_ID']\n[mcp_servers.remoteops.env]\nTOKEN = 'do-not-print-me'\n"
            );
            let inspected = inspect_source(&source).unwrap();
            assert!(inspected["args_config"].is_null());
            assert!(inspected["command_mode"].is_null());
            assert_eq!(inspected["legacy_env"], true);
            assert_eq!(inspected.as_object().unwrap().len(), 7);
            assert!(!inspected.to_string().contains("do-not-print-me"));
            assert!(!inspected.to_string().contains("/private"));
        }
    }

    #[test]
    fn inspector_does_not_accept_a_fake_table_as_the_real_entry() {
        assert!(
            inspect_source(
                "[mcp_servers.other]\ntext = '''\n[mcp_servers.remoteops]\ncommand = 'fake'\n'''\n"
            )
            .is_err()
        );
    }

    #[test]
    fn inspector_rejects_unexpected_environment_forwarding() {
        for env in [
            "[]",
            "['UNKNOWN']",
            "'not-an-array'",
            "['REMOTEOPS_CONTROLLER_TOKEN']",
        ] {
            let source = format!("[mcp_servers.remoteops]\nenv_vars = {env}\n");
            assert!(inspect_source(&source).is_err());
        }
    }

    #[test]
    fn concurrent_installer_lock_prevents_mutation() {
        let directory = temp_directory();
        let path = directory.join("config.toml");
        fs::write(&path, "model = 'keep'\n").unwrap();
        let lock = lock_config(&path).unwrap();
        assert!(configure(&path, "mcp", "mcp.json", "agent-controlled", false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "model = 'keep'\n");
        drop(lock);
        configure(&path, "mcp", "mcp.json", "agent-controlled", false).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn preserves_config_permissions_and_rejects_symbolic_links() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = temp_directory();
        let path = directory.join("config.toml");
        fs::write(&path, "model = 'keep'\n").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o640)).unwrap();
        configure(&path, "mcp", "mcp.json", "agent-controlled", false).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let link = directory.join("linked.toml");
        symlink(&path, &link).unwrap();
        assert!(configure(&link, "different", "mcp.json", "agent-controlled", false).is_err());
        assert!(validate(&link).is_err());
        assert!(unconfigure(&link).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
