use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::Parser;
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde_json::{Map, Value, json};
use tokio::process::Command;

/// 通过官方 Rust MCP SDK 验证 `RemoteOps` STDIO Server。
#[derive(Debug, Parser)]
struct Args {
    #[arg(long)]
    mcp_executable: PathBuf,
    #[arg(long)]
    human_cli_executable: PathBuf,
    #[arg(long)]
    relay: String,
    #[arg(long)]
    server_name: String,
    #[arg(long)]
    ca_cert: Option<PathBuf>,
    #[arg(long)]
    audit_log: PathBuf,
    #[arg(long)]
    transfer_root: PathBuf,
    #[arg(long, env = "REMOTEOPS_CONTROLLER_TOKEN", hide_env_values = true)]
    controller_token: String,
    #[arg(long, env = "REMOTEOPS_HUMAN_CONTROLLER_TOKEN", hide_env_values = true)]
    human_controller_token: String,
    #[arg(long, required = true)]
    pair: Vec<String>,
    #[arg(long)]
    upload_source: PathBuf,
    #[arg(long)]
    remote_file: String,
    #[arg(long)]
    download_target: PathBuf,
    #[arg(long, default_value = "127.0.0.1")]
    probe_host: String,
    #[arg(long)]
    probe_port: u16,
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tokio::fs::create_dir_all(&args.transfer_root)
        .await
        .context("创建 MCP transfer-root 失败")?;
    let transfer_root = tokio::fs::canonicalize(&args.transfer_root)
        .await
        .context("解析 MCP transfer-root 失败")?;
    let upload_source = tokio::fs::canonicalize(&args.upload_source)
        .await
        .context("解析 MCP 上传源失败")?;
    let upload_relative = upload_source
        .strip_prefix(&transfer_root)
        .context("MCP 上传源不在 transfer-root 内")?
        .to_string_lossy()
        .replace('\\', "/");
    let download_parent = args
        .download_target
        .parent()
        .context("MCP 下载目标缺少父目录")?;
    tokio::fs::create_dir_all(download_parent)
        .await
        .context("创建 MCP 下载目录失败")?;
    let download_parent = tokio::fs::canonicalize(download_parent)
        .await
        .context("解析 MCP 下载目录失败")?;
    let download_relative = download_parent
        .strip_prefix(&transfer_root)
        .context("MCP 下载目标不在 transfer-root 内")?
        .join(
            args.download_target
                .file_name()
                .context("MCP 下载目标缺少文件名")?,
        )
        .to_string_lossy()
        .replace('\\', "/");
    let readonly_client = start_mcp(&args, &transfer_root, None).await?;
    assert_remoteops_discovery_guidance(&readonly_client)?;
    let readonly_tools = readonly_client.list_all_tools().await?;
    assert_pairing_tool_guidance(&readonly_tools)?;
    let readonly_run_command = readonly_tools
        .iter()
        .find(|tool| tool.name.as_ref() == "run_command")
        .context("默认只读模式未注册 run_command")?;
    assert_read_only_annotation(&readonly_tools, "run_command", false)?;
    if readonly_run_command
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.destructive_hint)
        != Some(true)
    {
        bail!("run_command 未声明 destructive_hint=true");
    }
    call_expect_error(
        &readonly_client,
        "run_command",
        json!({
            "session_id": "00000000-0000-0000-0000-000000000000",
            "shell": "power_shell",
            "command": "'MUST_NOT_RUN'",
            "approval_id": "00000000-0000-0000-0000-000000000000"
        }),
        "默认只读模式下的非只读命令",
    )
    .await?;
    readonly_client.cancel().await?;

    let client = start_mcp(&args, &transfer_root, Some("approval")).await?;

    let tools = client.list_all_tools().await?;
    let tool_names: Vec<String> = tools.iter().map(|tool| tool.name.to_string()).collect();
    let required = [
        "list_connections",
        "pair_connection",
        "get_control_mode",
        "set_control_mode",
        "get_target_info",
        "run_readonly_command",
        "run_command",
        "test_port",
        "open_shell",
        "close_shell",
        "open_serial",
        "read_output",
        "upload_file",
        "download_file",
        "run_ssh",
        "clear_ssh_credential_cache",
        "request_action_approval",
        "close_connection",
    ];
    for tool in required {
        if !tool_names.iter().any(|name| name == tool) {
            bail!("MCP 缺少工具：{tool}");
        }
    }
    let readonly_tools = [
        "list_connections",
        "get_control_mode",
        "get_target_info",
        "run_readonly_command",
        "test_port",
        "read_output",
    ];
    let write_tools = [
        "pair_connection",
        "set_control_mode",
        "open_shell",
        "close_shell",
        "run_command",
        "open_serial",
        "upload_file",
        "download_file",
        "run_ssh",
        "clear_ssh_credential_cache",
        "request_action_approval",
        "close_connection",
    ];
    for name in readonly_tools {
        assert_read_only_annotation(&tools, name, true)?;
    }
    for name in write_tools {
        assert_read_only_annotation(&tools, name, false)?;
    }
    for name in ["run_command", "upload_file", "download_file"] {
        let tool = tools
            .iter()
            .find(|tool| tool.name.as_ref() == name)
            .with_context(|| format!("没有找到 MCP 工具 {name}"))?;
        if tool
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.destructive_hint)
            != Some(true)
        {
            bail!("MCP 工具 {name} 未声明 destructive_hint=true");
        }
    }
    let approval_tool = tools
        .iter()
        .find(|tool| tool.name.as_ref() == "request_action_approval")
        .context("没有找到 MCP 工具 request_action_approval")?;
    if approval_tool
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.destructive_hint)
        != Some(false)
    {
        bail!("审批申请本身不应声明 destructive_hint=true");
    }
    assert_approval_tool_cannot_decide(approval_tool)?;
    for tool in &tools {
        let schema = serde_json::to_string(&tool.input_schema)?;
        if schema.contains("\"password\"") {
            bail!("MCP 工具 {} 的输入架构暴露了 password 字段", tool.name);
        }
    }

    for pair in &args.pair {
        let (pairing_code, alias) = pair
            .split_once('=')
            .map_or((pair.as_str(), None), |(code, alias)| (code, Some(alias)));
        let paired = call(
            &client,
            "pair_connection",
            json!({
                "pairing_code": pairing_code,
                "alias": alias
            }),
        )
        .await?;
        if paired.get("session_id").and_then(Value::as_str).is_none() {
            bail!("pair_connection 未返回 session_id：{paired}");
        }
    }

    let list = call(&client, "list_connections", json!({})).await?;
    let connection_count = list
        .get("connections")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if connection_count != args.pair.len() {
        bail!(
            "list_connections 返回 {connection_count} 个连接，预期 {} 个",
            args.pair.len()
        );
    }
    let session_id = list
        .pointer("/connections/0/session_id")
        .and_then(Value::as_str)
        .context("list_connections 未返回 session_id")?
        .to_owned();
    let info = call(
        &client,
        "get_target_info",
        json!({ "session_id": session_id }),
    )
    .await?;
    if info.get("session_id").and_then(Value::as_str).is_none() {
        bail!("get_target_info 未返回连接信息");
    }

    let shell = call(
        &client,
        "open_shell",
        json!({
            "session_id": session_id,
            "shell": if cfg!(windows) { "power_shell" } else { "system" }
        }),
    )
    .await?;
    let shell_id = shell
        .get("shell_id")
        .and_then(Value::as_str)
        .context("open_shell 未返回 shell_id")?
        .to_owned();
    let command = if cfg!(windows) {
        "Write-Output 'REMOTEOPS_MCP_E2E'"
    } else {
        "printf REMOTEOPS_MCP_E2E"
    };
    let command_result = call(
        &client,
        "run_readonly_command",
        json!({
            "session_id": session_id,
            "shell": if cfg!(windows) { "windows_power_shell" } else { "system" },
            "command": command
        }),
    )
    .await?;
    if !command_result
        .get("summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| summary.contains("REMOTEOPS_MCP_E2E"))
    {
        bail!("MCP 只读命令输出不正确：{command_result}");
    }

    let approval_command = if cfg!(windows) {
        "$global:RemoteOpsMcpApprovalSmoke = 'REMOTEOPS_MCP_APPROVAL_E2E'; $global:RemoteOpsMcpApprovalSmoke"
    } else {
        "export REMOTEOPS_MCP_APPROVAL_SMOKE=REMOTEOPS_MCP_APPROVAL_E2E; printf $REMOTEOPS_MCP_APPROVAL_SMOKE"
    };
    call_expect_error(
        &client,
        "run_command",
        json!({
            "session_id": session_id,
            "shell_id": shell_id,
            "command": approval_command,
            "approval_id": null
        }),
        "缺少独立人工审批的非只读命令",
    )
    .await?;
    let command_approval_request = call(
        &client,
        "request_action_approval",
        json!({
            "session_id": session_id,
            "action": {
                "type": "run_command",
                "shell_id": shell_id,
                "command": approval_command
            }
        }),
    )
    .await?;
    if command_approval_request
        .get("status")
        .and_then(Value::as_str)
        != Some("pending")
    {
        bail!("命令审批申请没有进入 pending：{command_approval_request}");
    }
    let command_approval_id = command_approval_request
        .get("approval_id")
        .and_then(Value::as_str)
        .context("命令审批申请没有返回 approval_id")?
        .to_owned();
    let command_operation = command_approval_request
        .pointer("/details/operation")
        .cloned()
        .context("命令审批申请没有返回完整 operation")?;
    let pending_command = call(
        &client,
        "run_command",
        json!({
            "session_id": session_id,
            "shell_id": shell_id,
            "command": approval_command,
            "approval_id": command_approval_id
        }),
    )
    .await?;
    assert_not_completed(&pending_command, "尚未由 Human Controller 批准的命令")?;

    let command_human_approval = approve_with_human_cli(
        &args,
        &transfer_root,
        &session_id,
        &command_approval_id,
        &command_operation,
    )
    .await?;
    if command_human_approval.get("state").and_then(Value::as_str) != Some("approved") {
        bail!("独立 Human Controller 没有批准命令：{command_human_approval}");
    }

    let tampered_command = format!("{approval_command}; 'REMOTEOPS_MCP_TAMPERED'");
    let tampered_result = call(
        &client,
        "run_command",
        json!({
            "session_id": session_id,
            "shell_id": shell_id,
            "command": tampered_command,
            "approval_id": command_approval_id
        }),
    )
    .await?;
    assert_not_completed(&tampered_result, "被篡改的命令")?;

    let approved_command_result = call(
        &client,
        "run_command",
        json!({
            "session_id": session_id,
            "shell_id": shell_id,
            "command": approval_command,
            "approval_id": command_approval_id
        }),
    )
    .await?;
    if !approved_command_result
        .get("summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| summary.contains("REMOTEOPS_MCP_APPROVAL_E2E"))
    {
        bail!("经独立人工批准后的命令输出不正确：{approved_command_result}");
    }

    let persistent_read_command = if cfg!(windows) {
        "Get-Variable -Name RemoteOpsMcpApprovalSmoke -ValueOnly"
    } else {
        "printf $REMOTEOPS_MCP_APPROVAL_SMOKE"
    };
    let persistent_read_approval = call(
        &client,
        "request_action_approval",
        json!({
            "session_id": session_id,
            "action": {
                "type": "run_command",
                "shell_id": shell_id,
                "command": persistent_read_command
            }
        }),
    )
    .await?;
    let persistent_read_approval_id = persistent_read_approval
        .get("approval_id")
        .and_then(Value::as_str)
        .context("持久 Shell 读取审批申请没有返回 approval_id")?
        .to_owned();
    let persistent_read_operation = persistent_read_approval
        .pointer("/details/operation")
        .cloned()
        .context("持久 Shell 读取审批申请没有返回完整 operation")?;
    let persistent_read_human_approval = approve_with_human_cli(
        &args,
        &transfer_root,
        &session_id,
        &persistent_read_approval_id,
        &persistent_read_operation,
    )
    .await?;
    if persistent_read_human_approval
        .get("state")
        .and_then(Value::as_str)
        != Some("approved")
    {
        bail!("独立 Human Controller 没有批准持久 Shell 读取：{persistent_read_human_approval}");
    }
    let persistent_state = call(
        &client,
        "run_command",
        json!({
            "session_id": session_id,
            "shell_id": shell_id,
            "command": persistent_read_command,
            "approval_id": persistent_read_approval_id
        }),
    )
    .await?;
    if !persistent_state
        .get("summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| summary.contains("REMOTEOPS_MCP_APPROVAL_E2E"))
    {
        bail!("持久 PowerShell 状态未保留：{persistent_state}");
    }

    let reused_command_approval = call(
        &client,
        "run_command",
        json!({
            "session_id": session_id,
            "shell_id": shell_id,
            "command": approval_command,
            "approval_id": command_approval_id
        }),
    )
    .await?;
    assert_not_completed(&reused_command_approval, "重复使用 approval_id 的命令")?;

    let closed_shell = call(
        &client,
        "close_shell",
        json!({
            "session_id": session_id,
            "shell_id": shell_id
        }),
    )
    .await?;
    if closed_shell.get("status").and_then(Value::as_str) != Some("completed") {
        bail!("MCP 持久 Shell 关闭失败：{closed_shell}");
    }

    let port_result = call(
        &client,
        "test_port",
        json!({
            "session_id": session_id,
            "host": args.probe_host,
            "port": args.probe_port
        }),
    )
    .await?;
    if !port_result
        .get("summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| summary.contains("open"))
    {
        bail!("MCP 端口探测失败：{port_result}");
    }

    call_expect_error(
        &client,
        "upload_file",
        json!({
            "session_id": session_id,
            "local_path": "../outside-transfer-root.txt",
            "remote_path": args.remote_file,
            "overwrite": false,
            "approval_id": null
        }),
        "越界文件路径",
    )
    .await?;

    call_expect_error(
        &client,
        "request_action_approval",
        json!({
            "session_id": session_id,
            "approval_id": "00000000-0000-0000-0000-000000000000",
            "approved": true
        }),
        "MCP 同一客户端自批准",
    )
    .await?;

    let approval_request = call(
        &client,
        "request_action_approval",
        json!({
            "session_id": session_id,
            "action": {
                "type": "upload_file",
                "local_path": upload_relative,
                "remote_path": args.remote_file,
                "overwrite": false
            }
        }),
    )
    .await?;
    if approval_request.get("status").and_then(Value::as_str) != Some("pending") {
        bail!("审批申请没有进入 pending：{approval_request}");
    }
    let approval_id = approval_request
        .get("approval_id")
        .and_then(Value::as_str)
        .context("审批申请没有返回 approval_id")?
        .to_owned();
    let operation = approval_request
        .pointer("/details/operation")
        .cloned()
        .context("审批申请没有返回完整 operation")?;

    let unapproved_upload = call(
        &client,
        "upload_file",
        json!({
            "session_id": session_id,
            "local_path": upload_relative,
            "remote_path": args.remote_file,
            "overwrite": false,
            "approval_id": approval_id
        }),
    )
    .await?;
    if unapproved_upload.get("status").and_then(Value::as_str) == Some("completed") {
        bail!("尚未由独立 Human Controller 批准的审批被错误执行：{unapproved_upload}");
    }

    let human_approval =
        approve_with_human_cli(&args, &transfer_root, &session_id, &approval_id, &operation)
            .await?;
    if human_approval.get("state").and_then(Value::as_str) != Some("approved") {
        bail!("独立 Human Controller 没有批准操作：{human_approval}");
    }

    let upload = call(
        &client,
        "upload_file",
        json!({
            "session_id": session_id,
            "local_path": upload_relative,
            "remote_path": args.remote_file,
            "overwrite": false,
            "approval_id": approval_id
        }),
    )
    .await?;
    if upload.get("status").and_then(Value::as_str) != Some("completed") {
        bail!("经独立人工批准后 MCP 上传失败：{upload}");
    }

    let reused_approval = call(
        &client,
        "upload_file",
        json!({
            "session_id": session_id,
            "local_path": upload_relative,
            "remote_path": args.remote_file,
            "overwrite": false,
            "approval_id": approval_id
        }),
    )
    .await?;
    if reused_approval.get("status").and_then(Value::as_str) == Some("completed") {
        bail!("同一 approval_id 被重复消费：{reused_approval}");
    }

    let download_approval_request = call(
        &client,
        "request_action_approval",
        json!({
            "session_id": session_id,
            "action": {
                "type": "download_file",
                "remote_path": args.remote_file,
                "overwrite_local": true
            }
        }),
    )
    .await?;
    let download_approval_id = download_approval_request
        .get("approval_id")
        .and_then(Value::as_str)
        .context("覆盖本地下载审批没有返回 approval_id")?
        .to_owned();
    let download_operation = download_approval_request
        .pointer("/details/operation")
        .cloned()
        .context("覆盖本地下载审批没有返回完整 operation")?;
    call_expect_error(
        &client,
        "download_file",
        json!({
            "session_id": session_id,
            "remote_path": args.remote_file,
            "local_path": download_relative,
            "overwrite_local": true
        }),
        "未经审批的本地覆盖下载",
    )
    .await?;
    approve_with_human_cli(
        &args,
        &transfer_root,
        &session_id,
        &download_approval_id,
        &download_operation,
    )
    .await?;
    let download = call(
        &client,
        "download_file",
        json!({
            "session_id": session_id,
            "remote_path": args.remote_file,
            "local_path": download_relative,
            "overwrite_local": true,
            "approval_id": download_approval_id
        }),
    )
    .await?;
    if download.get("status").and_then(Value::as_str) != Some("completed") {
        bail!("MCP 下载失败：{download}");
    }
    let uploaded_bytes = tokio::fs::read(&upload_source)
        .await
        .context("读取 MCP 上传源进行最终校验失败")?;
    let downloaded_bytes = tokio::fs::read(&args.download_target)
        .await
        .context("读取 MCP 下载结果进行最终校验失败")?;
    if downloaded_bytes != uploaded_bytes {
        bail!("MCP 上传下载后的文件内容不一致");
    }

    let events = call(
        &client,
        "read_output",
        json!({
            "session_id": session_id,
            "after_sequence": null,
            "limit": 100
        }),
    )
    .await?;
    if events
        .get("events")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        bail!("read_output 没有返回统一事件");
    }

    let close = call(
        &client,
        "close_connection",
        json!({ "session_id": session_id }),
    )
    .await?;
    if close.get("status").and_then(Value::as_str) != Some("completed") {
        bail!("close_connection 失败：{close}");
    }
    let list_after_close = call(&client, "list_connections", json!({})).await?;
    if list_after_close
        .get("connections")
        .and_then(Value::as_array)
        .is_none_or(|connections| !connections.is_empty())
    {
        bail!("close_connection 返回后连接列表仍不为空：{list_after_close}");
    }
    client.cancel().await?;

    let agent_controlled_client = start_mcp(&args, &transfer_root, None).await?;
    let first_pair = args.pair.first().context("缺少接管测试配对码")?;
    let (pairing_code, alias) = first_pair
        .split_once('=')
        .map_or((first_pair.as_str(), None), |(code, alias)| {
            (code, Some(alias))
        });
    let paired = call(
        &agent_controlled_client,
        "pair_connection",
        json!({
            "pairing_code": pairing_code,
            "alias": alias
        }),
    )
    .await?;
    let agent_controlled_session_id = paired
        .get("session_id")
        .and_then(Value::as_str)
        .context("普通 MCP 配对未返回 session_id")?
        .to_owned();
    let step_by_step = call(
        &agent_controlled_client,
        "get_control_mode",
        json!({ "session_id": agent_controlled_session_id }),
    )
    .await?;
    if step_by_step.get("mode").and_then(Value::as_str) != Some("step_by_step") {
        bail!("普通 MCP 配对后没有默认使用逐项确认：{step_by_step}");
    }
    let step_by_step_shell = call(
        &agent_controlled_client,
        "open_shell",
        json!({
            "session_id": agent_controlled_session_id,
            "shell": if cfg!(windows) { "power_shell" } else { "system" }
        }),
    )
    .await?;
    let step_by_step_shell_id = step_by_step_shell
        .get("shell_id")
        .and_then(Value::as_str)
        .context("逐项确认模式未能打开持久 Shell")?;
    let full_access_command = if cfg!(windows) {
        "Write-Output 'REMOTEOPS_FULL_ACCESS_E2E'"
    } else {
        "printf REMOTEOPS_FULL_ACCESS_E2E"
    };
    call_expect_error(
        &agent_controlled_client,
        "run_command",
        json!({
            "session_id": agent_controlled_session_id,
            "shell_id": step_by_step_shell_id,
            "command": full_access_command,
            "approval_id": null
        }),
        "逐项确认模式下未经当前用户确认的持久 Shell 命令",
    )
    .await?;
    let full_access = call(
        &agent_controlled_client,
        "set_control_mode",
        json!({
            "session_id": agent_controlled_session_id,
            "mode": "full_access"
        }),
    )
    .await?;
    if full_access.get("mode").and_then(Value::as_str) != Some("full_access") {
        bail!("set_control_mode 没有直接开启完全控制：{full_access}");
    }
    let full_access_result = call(
        &agent_controlled_client,
        "run_command",
        json!({
            "session_id": agent_controlled_session_id,
            "shell_id": step_by_step_shell_id,
            "command": full_access_command,
            "approval_id": null
        }),
    )
    .await?;
    if full_access_result.get("status").and_then(Value::as_str) != Some("completed")
        || !full_access_result
            .get("summary")
            .and_then(Value::as_str)
            .is_some_and(|summary| summary.contains("REMOTEOPS_FULL_ACCESS_E2E"))
    {
        bail!("完全控制未能直接执行持久 Shell 命令：{full_access_result}");
    }

    let takeover_client = start_mcp(&args, &transfer_root, None).await?;
    let takeover_pair = call(
        &takeover_client,
        "pair_connection",
        json!({
            "pairing_code": pairing_code,
            "alias": alias
        }),
    )
    .await?;
    let takeover_session_id = takeover_pair
        .get("session_id")
        .and_then(Value::as_str)
        .context("同 Owner MCP 接管未返回 session_id")?
        .to_owned();
    let mut old_connection_cleared = false;
    for _ in 0..20 {
        let old_list = call(&agent_controlled_client, "list_connections", json!({})).await?;
        old_connection_cleared = old_list
            .get("connections")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
        if old_connection_cleared {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if !old_connection_cleared {
        bail!("同 Owner MCP 接管后旧 MCP 连接列表没有清空");
    }
    let takeover_mode = call(
        &takeover_client,
        "get_control_mode",
        json!({ "session_id": takeover_session_id }),
    )
    .await?;
    if takeover_mode.get("mode").and_then(Value::as_str) != Some("step_by_step") {
        bail!("同 Owner MCP 接管后错误继承了完全控制：{takeover_mode}");
    }
    let takeover_close = call(
        &takeover_client,
        "close_connection",
        json!({ "session_id": takeover_session_id }),
    )
    .await?;
    if takeover_close.get("status").and_then(Value::as_str) != Some("completed") {
        bail!("同 Owner MCP 接管后无法显式断开：{takeover_close}");
    }
    let takeover_list_after_close = call(&takeover_client, "list_connections", json!({})).await?;
    if takeover_list_after_close
        .get("connections")
        .and_then(Value::as_array)
        .is_none_or(|connections| !connections.is_empty())
    {
        bail!("接管 MCP 显式断开后连接列表仍不为空：{takeover_list_after_close}");
    }
    agent_controlled_client.cancel().await?;
    takeover_client.cancel().await?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "tools": tool_names,
            "tool_annotations_verified": true,
            "approval_request_only_schema_verified": true,
            "same_mcp_self_approval_rejected": true,
            "readonly_command_mode_rejected": true,
            "approval_command_mode_verified": true,
            "command_approval_exact_binding_verified": true,
            "command_approval_consumed_once_verified": true,
            "persistent_power_shell_verified": cfg!(windows),
            "default_step_by_step_verified": true,
            "full_access_without_nested_confirmation_verified": true,
            "same_owner_takeover_verified": true,
            "takeover_resets_full_access_verified": true,
            "close_clears_connection_list_verified": true,
            "unapproved_operation_rejected": true,
            "independent_human_approval_verified": true,
            "approval_consumed_once_verified": true,
            "download_content_verified": true,
            "transfer_root_escape_rejected": true,
            "session_id": session_id,
            "command": command_result,
            "command_approval_request": command_approval_request,
            "command_human_approval": command_human_approval,
            "tampered_command": tampered_result,
            "approved_command": approved_command_result,
            "persistent_state": persistent_state,
            "reused_command_approval": reused_command_approval,
            "port": port_result,
            "approval_request": approval_request,
            "unapproved_upload": unapproved_upload,
            "human_approval": human_approval,
            "upload": upload,
            "reused_approval": reused_approval,
            "download": download,
            "download_target": args.download_target,
            "event_count": events["events"].as_array().map_or(0, Vec::len),
            "close": close
        }))?
    );
    Ok(())
}

async fn start_mcp(
    args: &Args,
    transfer_root: &std::path::Path,
    command_mode: Option<&str>,
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ()>> {
    let executable = args.mcp_executable.clone();
    let client = ()
        .serve(TokioChildProcess::new(
            Command::new(&executable).configure(|command| {
                command
                    .arg("--relay")
                    .arg(&args.relay)
                    .arg("--server-name")
                    .arg(&args.server_name);
                if let Some(ca_cert) = &args.ca_cert {
                    command.arg("--ca-cert").arg(ca_cert);
                }
                command
                    .arg("--audit-log")
                    .arg(&args.audit_log)
                    .arg("--transfer-root")
                    .arg(transfer_root)
                    .env_remove("REMOTEOPS_COMMAND_MODE")
                    .env_remove("REMOTEOPS_HUMAN_CONTROLLER_TOKEN")
                    .env("REMOTEOPS_CONTROLLER_TOKEN", &args.controller_token);
                if let Some(command_mode) = command_mode {
                    command.arg("--command-mode").arg(command_mode);
                }
            }),
        )?)
        .await
        .with_context(|| format!("启动 MCP Server 失败：{}", executable.display()))?;
    Ok(client)
}

fn assert_not_completed(result: &Value, expectation: &str) -> anyhow::Result<()> {
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .with_context(|| format!("{expectation}没有返回 status：{result}"))?;
    if status == "completed" {
        bail!("{expectation}被错误执行：{result}");
    }
    Ok(())
}

fn assert_read_only_annotation(
    tools: &[rmcp::model::Tool],
    name: &str,
    expected: bool,
) -> anyhow::Result<()> {
    let tool = tools
        .iter()
        .find(|tool| tool.name.as_ref() == name)
        .with_context(|| format!("没有找到 MCP 工具 {name}"))?;
    let actual = tool
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.read_only_hint);
    if actual != Some(expected) {
        bail!("MCP 工具 {name} 的 read_only_hint 为 {actual:?}，预期 {expected}");
    }
    Ok(())
}

fn assert_remoteops_discovery_guidance(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
) -> anyhow::Result<()> {
    let peer_info = client
        .peer_info()
        .context("MCP initialize 未返回服务端信息")?;
    let instructions = peer_info
        .instructions
        .as_deref()
        .context("MCP initialize 未返回 RemoteOps 使用说明")?;
    for required in [
        "必须先调用 pair_connection",
        "配对后默认逐项确认",
        "Codex 对该工具的授权就是唯一确认",
        "Agent 端没有逐项确认或完全控制按钮",
        "按 session_id 独立保存在 MCP 内存",
        "禁止改用 Computer Use",
        "检查、分析、判断等请求默认只读",
        "禁止声称已操作远端",
    ] {
        if !instructions.contains(required) {
            bail!("MCP 使用说明缺少关键路由规则：{required}");
        }
    }
    Ok(())
}

fn assert_pairing_tool_guidance(tools: &[rmcp::model::Tool]) -> anyhow::Result<()> {
    let tool = tools
        .iter()
        .find(|tool| tool.name.as_ref() == "pair_connection")
        .context("没有找到 MCP 工具 pair_connection")?;
    let description = tool
        .description
        .as_deref()
        .context("pair_connection 缺少工具描述")?;
    for required in ["RemoteOps 控制码", "必须先调用此工具", "不要改用远程桌面"]
    {
        if !description.contains(required) {
            bail!("pair_connection 描述缺少关键触发规则：{required}");
        }
    }
    Ok(())
}

async fn approve_with_human_cli(
    args: &Args,
    transfer_root: &std::path::Path,
    session_id: &str,
    approval_id: &str,
    operation: &Value,
) -> anyhow::Result<Value> {
    let operation_json = serde_json::to_string(operation).context("序列化人工审批操作失败")?;
    let mut command = Command::new(&args.human_cli_executable);
    command
        .arg("--relay")
        .arg(&args.relay)
        .arg("--server-name")
        .arg(&args.server_name);
    if let Some(ca_cert) = &args.ca_cert {
        command.arg("--ca-cert").arg(ca_cert);
    }
    command
        .arg("--audit-log")
        .arg(transfer_root.join("human-approval-audit.jsonl"))
        .args(args.pair.iter().flat_map(|pair| ["--pair", pair.as_str()]))
        .arg("--json")
        .arg("approval-decide")
        .arg(session_id)
        .arg(approval_id)
        .arg(operation_json)
        .env(
            "REMOTEOPS_HUMAN_CONTROLLER_TOKEN",
            &args.human_controller_token,
        );
    let output = command.output().await.with_context(|| {
        format!(
            "启动独立 Human Controller CLI 失败：{}",
            args.human_cli_executable.display()
        )
    })?;
    if !output.status.success() {
        bail!(
            "独立 Human Controller CLI 审批失败（exit={}）：{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let result: Value =
        serde_json::from_slice(&output.stdout).context("Human Controller CLI 未返回有效 JSON")?;
    if result.get("approval_id").and_then(Value::as_str) != Some(approval_id) {
        bail!("Human Controller CLI 返回了不匹配的 approval_id：{result}");
    }
    Ok(result)
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    arguments: Value,
) -> anyhow::Result<Value> {
    let arguments: Map<String, Value> = arguments
        .as_object()
        .cloned()
        .context("工具参数必须是 JSON 对象")?;
    let result = client
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments))
        .await?;
    if result.is_error == Some(true) {
        bail!("MCP 工具 {name} 返回错误：{result:?}");
    }
    result
        .structured_content
        .context("MCP 工具未返回 structuredContent")
}

async fn call_expect_error(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    arguments: Value,
    expectation: &str,
) -> anyhow::Result<()> {
    let arguments: Map<String, Value> = arguments
        .as_object()
        .cloned()
        .context("工具参数必须是 JSON 对象")?;
    let result = client
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments))
        .await?;
    if result.is_error != Some(true) {
        bail!("MCP 工具 {name} 应拒绝{expectation}，实际结果：{result:?}");
    }
    Ok(())
}

fn assert_approval_tool_cannot_decide(tool: &rmcp::model::Tool) -> anyhow::Result<()> {
    let schema = serde_json::to_value(&tool.input_schema).context("序列化审批工具输入架构失败")?;
    let schema_text = serde_json::to_string(&schema)?;
    for forbidden in [
        "\"approved\"",
        "\"authorization\"",
        "\"authorization_expires_at\"",
    ] {
        if schema_text.contains(forbidden) {
            bail!("request_action_approval 输入架构仍暴露决定能力：{forbidden}");
        }
    }
    for required in ["\"session_id\"", "\"action\""] {
        if !schema_text.contains(required) {
            bail!("request_action_approval 输入架构缺少申请字段：{required}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ca_smoke_does_not_require_ca_cert_argument() {
        let args = Args::try_parse_from([
            "remoteops-mcp-smoke",
            "--mcp-executable",
            "mcp.exe",
            "--human-cli-executable",
            "cli.exe",
            "--relay",
            "relay.example.com:7443",
            "--server-name",
            "relay.example.com",
            "--audit-log",
            "audit.jsonl",
            "--transfer-root",
            "transfer",
            "--controller-token",
            "controller-token",
            "--human-controller-token",
            "human-token",
            "--pair",
            "123-456-789=test",
            "--upload-source",
            "upload.txt",
            "--remote-file",
            "remote.txt",
            "--download-target",
            "download.txt",
            "--probe-port",
            "7443",
        ])
        .expect("公网 CA 测试不应要求显式证书文件");

        assert!(args.ca_cert.is_none());
    }
}
