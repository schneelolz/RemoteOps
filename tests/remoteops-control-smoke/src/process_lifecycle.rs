//! 在专属回环 Relay 上验证 Agent 操作系统进程退出、重启与状态文件互斥。
use super::{Mcp, call, mkdir_args, raw_call, start_mcp};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{env, path::Path, process::Stdio, time::Duration};
use tokio::{
    process::{Child, Command},
    time::{sleep, timeout},
};

fn spawn_agent(root: &Path, address: &str, cert: &Path) -> Result<Child> {
    let executable = env::current_exe()?
        .parent()
        .context("验收程序缺少父目录")?
        .join(if cfg!(windows) {
            "remoteops-agent.exe"
        } else {
            "remoteops-agent"
        });
    ensure!(executable.is_file(), "请先构建同批 remoteops-agent CLI");
    let mut command = Command::new(executable);
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("REMOTEOPS_") {
            command.env_remove(key);
        }
    }
    command
        .arg("--config")
        .arg(root.join("agent-config.json"))
        .arg("--relay")
        .arg(address)
        .arg("--server-name")
        .arg("localhost")
        .arg("--ca-cert")
        .arg(cert)
        .arg("--state-file")
        .arg(root.join("agent-state.json"))
        .arg("--transfer-root")
        .arg(root.join("transfers"))
        .arg("--retry-seconds")
        .arg("1")
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command.spawn().context("启动隔离 Agent CLI 失败")
}

// 仅从脚本指定的专属 Relay 状态中读取测试身份；不输出状态、令牌或控制码。
fn registered_identity(root: &Path, relay_state: &Path) -> Result<Option<(String, String)>> {
    let local_path = root.join("agent-state.json");
    if !local_path.is_file() || !relay_state.is_file() {
        return Ok(None);
    }
    let local: Value = serde_json::from_slice(&std::fs::read(local_path)?)?;
    let Some(token) = local["resume_token"]
        .as_str()
        .filter(|token| !token.is_empty())
    else {
        return Ok(None);
    };
    let agent_id = local["agent_instance_id"]
        .as_str()
        .context("Agent 状态缺少身份")?;
    let relay: Value = serde_json::from_slice(&std::fs::read(relay_state)?)?;
    let Some(agent) = relay["agents"]
        .as_array()
        .context("测试 Relay 状态缺少身份列表")?
        .iter()
        .find(|agent| agent["hello"]["agent_instance_id"].as_str() == Some(agent_id))
    else {
        return Ok(None);
    };
    if agent["resume_token"].as_str() != Some(token) {
        return Ok(None);
    }
    let code = agent["lease"]["pairing_code"]
        .as_str()
        .context("测试身份缺少控制码")?;
    Ok(Some((agent_id.to_owned(), code.to_owned())))
}

async fn wait_paired(
    client: &Mcp,
    child: &mut Child,
    root: &Path,
    relay_state: &Path,
    previous_code: Option<&str>,
) -> Result<(String, String, String)> {
    timeout(Duration::from_secs(25), async {
        loop {
            ensure!(child.try_wait()?.is_none(), "隔离 Agent CLI 在配对前退出");
            if let Some((agent_id, code)) = registered_identity(root, relay_state)?
                && previous_code != Some(code.as_str())
            {
                let pair =
                    raw_call(client, "pair_connection", json!({"pairing_code":code})).await?;
                if pair.is_error != Some(true) {
                    let data = pair.structured_content.context("进程配对缺少结果")?;
                    let session = data["session_id"].as_str().context("进程配对缺少会话")?;
                    return Ok((agent_id, code, session.to_owned()));
                }
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("等待真实 Agent CLI 配对超时")?
}

async fn kill_and_wait(child: &mut Option<Child>) -> Result<()> {
    if let Some(mut process) = child.take() {
        if process.try_wait()?.is_none() {
            process.start_kill().context("终止测试 Agent 失败")?;
        }
        timeout(Duration::from_secs(10), process.wait())
            .await
            .context("等待测试 Agent 退出超时")??;
    }
    Ok(())
}

pub(super) async fn run(root: &Path, address: &str, cert: &Path) -> Result<()> {
    let relay_state = env::var_os("CONTROL_SMOKE_RELAY_STATE")
        .map(std::path::PathBuf::from)
        .context("缺少专属测试 Relay 状态路径，请通过 Test-ControlPermissions 脚本运行")?;
    std::fs::write(root.join("agent-config.json"), "{}")?;
    let client = start_mcp(root, address, cert).await?;
    let mut agent = None;
    let mut duplicate = None;
    let result = async {
        agent = Some(spawn_agent(root, address, cert)?);
        let (identity, old_code, old_session) = wait_paired(
            &client,
            agent.as_mut().context("缺少 Agent 子进程")?,
            root,
            &relay_state,
            None,
        )
        .await?;
        call(
            &client,
            "set_control_mode",
            json!({"session_id":old_session,"mode":"full_access"}),
        )
        .await?;

        duplicate = Some(spawn_agent(root, address, cert)?);
        let duplicate_exit = timeout(
            Duration::from_secs(10),
            duplicate.as_mut().context("缺少第二 Agent 子进程")?.wait(),
        )
        .await
        .context("同一状态文件的第二 Agent 未明确退出")??;
        ensure!(
            !duplicate_exit.success(),
            "同一状态文件允许第二 Agent 成功运行"
        );
        ensure!(
            agent
                .as_mut()
                .context("缺少原 Agent")?
                .try_wait()?
                .is_none(),
            "第二 Agent 影响了原进程"
        );
        let current = registered_identity(root, &relay_state)?.context("原身份意外丢失")?;
        ensure!(
            current == (identity.clone(), old_code.clone()),
            "第二 Agent 改变了原身份或控制码"
        );
        println!("通过：同一状态文件的第二个真实 Agent CLI 明确失败，原进程保持运行");

        kill_and_wait(&mut agent).await?;
        agent = Some(spawn_agent(root, address, cert)?);
        let (new_identity, new_code, new_session) = wait_paired(
            &client,
            agent.as_mut().context("缺少重启 Agent 子进程")?,
            root,
            &relay_state,
            Some(&old_code),
        )
        .await?;
        ensure!(
            identity == new_identity && old_code != new_code && old_session != new_session,
            "真实进程重启未保持身份并轮换控制码与会话"
        );
        let old_pair =
            raw_call(&client, "pair_connection", json!({"pairing_code":old_code})).await?;
        ensure!(
            old_pair.is_error == Some(true),
            "真实进程重启后旧控制码仍有效"
        );
        let denied = root.join("old-process-session-must-not-exist");
        let old_write =
            raw_call(&client, "run_command", mkdir_args(&old_session, &denied)?).await?;
        ensure!(
            old_write.is_error == Some(true) && !denied.exists(),
            "旧进程会话仍能执行写入"
        );
        super::assert_mcp_mode(&client, &new_session, "step_by_step", None).await?;
        println!(
            "通过：真实 Agent CLI 进程 kill/wait 后复用状态重启换码换会话，旧会话与临时完全控制失效"
        );
        Ok(())
    }
    .await;
    let duplicate_cleanup = kill_and_wait(&mut duplicate).await;
    let agent_cleanup = kill_and_wait(&mut agent).await;
    let _ = client.cancel().await;
    result?;
    duplicate_cleanup?;
    agent_cleanup?;
    Ok(())
}
