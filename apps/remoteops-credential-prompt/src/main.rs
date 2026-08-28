#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::io::{IsTerminal as _, Write as _};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::process::{Command, Stdio};

use anyhow::{Context as _, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use zeroize::Zeroize as _;
use zeroize::Zeroizing;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 22)]
    port: u16,
    #[arg(long)]
    username: String,
    #[arg(long)]
    command_sha256: String,
}

#[derive(Debug, Deserialize)]
struct PlatformResponse {
    action: String,
    #[serde(default)]
    password: String,
}

#[derive(Debug, Serialize)]
struct PromptResponse<'a> {
    action: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<&'a str>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("RemoteOps 凭据输入失败：{error:#}");
        std::process::exit(2);
    }
}

fn run() -> anyhow::Result<()> {
    let args = Args::parse();
    if std::io::stdout().is_terminal() {
        bail!("该内部程序只能由 RemoteOps MCP 通过受控管道启动");
    }
    if args.host.trim().is_empty() || args.username.trim().is_empty() {
        bail!("SSH 主机和用户名不能为空");
    }
    if args.command_sha256.len() != 64
        || !args
            .command_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("命令摘要无效");
    }
    let target = format!(
        "{}@{}:{}",
        args.username.trim(),
        args.host.trim(),
        args.port
    );
    let mut platform = platform_prompt(&target, &args.command_sha256)?;
    let action = match platform.action.as_str() {
        "use_once" => "use_once",
        "remember_10_minutes" => "remember_10_minutes",
        "cancel" => "cancel",
        _ => bail!("安全输入窗口返回了未知操作"),
    };
    if action != "cancel" && platform.password.is_empty() {
        bail!("SSH 密码不能为空");
    }
    let password = Zeroizing::new(std::mem::take(&mut platform.password));
    let response = PromptResponse {
        action,
        password: (action != "cancel").then_some(password.as_str()),
    };
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &response).context("无法编码安全输入结果")?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn platform_prompt(target: &str, command_sha256: &str) -> anyhow::Result<PlatformResponse> {
    use std::os::windows::process::CommandExt as _;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$inputJson = [Console]::In.ReadToEnd() | ConvertFrom-Json
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$form = New-Object Windows.Forms.Form
$form.Text = 'RemoteOps SSH credential'
$form.StartPosition = 'CenterScreen'
$form.TopMost = $true
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.MinimizeBox = $false
$form.ClientSize = New-Object Drawing.Size(470, 235)
$label = New-Object Windows.Forms.Label
$label.Location = New-Object Drawing.Point(20, 18)
$label.Size = New-Object Drawing.Size(430, 72)
$label.Text = "Enter the SSH password for:`r`n$($inputJson.target)`r`nCommand SHA-256: $($inputJson.command_sha256.Substring(0, 16))..."
$form.Controls.Add($label)
$password = New-Object Windows.Forms.TextBox
$password.Location = New-Object Drawing.Point(20, 98)
$password.Size = New-Object Drawing.Size(430, 28)
$password.UseSystemPasswordChar = $true
$form.Controls.Add($password)
$remember = New-Object Windows.Forms.CheckBox
$remember.Location = New-Object Drawing.Point(20, 137)
$remember.Size = New-Object Drawing.Size(300, 26)
$remember.Text = 'Remember in MCP memory for 10 minutes'
$remember.Checked = $false
$form.Controls.Add($remember)
$ok = New-Object Windows.Forms.Button
$ok.Location = New-Object Drawing.Point(268, 178)
$ok.Size = New-Object Drawing.Size(87, 32)
$ok.Text = 'Use'
$ok.DialogResult = [Windows.Forms.DialogResult]::OK
$form.AcceptButton = $ok
$form.Controls.Add($ok)
$cancel = New-Object Windows.Forms.Button
$cancel.Location = New-Object Drawing.Point(363, 178)
$cancel.Size = New-Object Drawing.Size(87, 32)
$cancel.Text = 'Cancel'
$cancel.DialogResult = [Windows.Forms.DialogResult]::Cancel
$form.CancelButton = $cancel
$form.Controls.Add($cancel)
$form.Add_Shown({
  $form.Activate()
  $form.BringToFront()
  $password.Focus()
  $password.Select()
})
$result = $form.ShowDialog()
if ($result -ne [Windows.Forms.DialogResult]::OK) {
  [Console]::Out.Write('{"action":"cancel","password":""}')
  exit 0
}
$action = if ($remember.Checked) { 'remember_10_minutes' } else { 'use_once' }
$plain = $password.Text
$password.Clear()
[Console]::Out.Write((@{ action = $action; password = $plain } | ConvertTo-Json -Compress))
"#;
    let input = serde_json::json!({
        "target": target,
        "command_sha256": command_sha256,
    });
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-Command",
            SCRIPT,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("无法启动 Windows 安全输入窗口")?;
    serde_json::to_writer(
        child.stdin.as_mut().context("安全输入窗口 stdin 不可用")?,
        &input,
    )?;
    drop(child.stdin.take());
    decode_platform_response(child.wait_with_output()?)
}

#[cfg(target_os = "macos")]
fn platform_prompt(target: &str, command_sha256: &str) -> anyhow::Result<PlatformResponse> {
    const SCRIPT: &str = r#"
on run argv
  set targetName to item 1 of argv
  set commandHash to item 2 of argv
  try
    set response to display dialog "Enter the SSH password for:" & return & targetName & return & "Command SHA-256: " & text 1 thru 16 of commandHash & "..." default answer "" with hidden answer buttons {"Cancel", "Use once", "Remember 10 minutes"} default button "Use once" cancel button "Cancel" with title "RemoteOps SSH credential"
    set selectedButton to button returned of response
    set secretText to text returned of response
    if selectedButton is "Remember 10 minutes" then
      return "remember_10_minutes" & linefeed & secretText
    end if
    return "use_once" & linefeed & secretText
  on error number -128
    return "cancel" & linefeed
  end try
end run
"#;
    let output = Command::new("osascript")
        .args(["-e", SCRIPT, "--", target, command_sha256])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("无法启动 macOS 安全输入窗口")?;
    if !output.status.success() {
        bail!(
            "macOS 安全输入窗口失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut text =
        Zeroizing::new(String::from_utf8(output.stdout).context("安全输入结果不是 UTF-8")?);
    let (action, password) = text
        .split_once('\n')
        .map_or((text.trim(), ""), |(action, password)| {
            (action.trim(), password.trim_end())
        });
    let response = PlatformResponse {
        action: action.to_owned(),
        password: password.to_owned(),
    };
    text.zeroize();
    Ok(response)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn platform_prompt(_target: &str, _command_sha256: &str) -> anyhow::Result<PlatformResponse> {
    bail!("SSH 密码安全输入仅支持 Windows 和 macOS Controller")
}

#[cfg(target_os = "windows")]
fn decode_platform_response(output: std::process::Output) -> anyhow::Result<PlatformResponse> {
    if !output.status.success() {
        bail!(
            "Windows 安全输入窗口失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut text =
        Zeroizing::new(String::from_utf8(output.stdout).context("安全输入结果不是 UTF-8")?);
    let response = serde_json::from_str(text.trim()).context("安全输入结果格式无效")?;
    text.zeroize();
    Ok(response)
}
