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
    /// 界面语言；未指定时跟随系统语言。
    #[arg(long, env = "REMOTEOPS_LANG")]
    language: Option<String>,
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
    let language = prompt_language(args.language.as_deref());
    let mut platform = platform_prompt(&target, &args.command_sha256, language)?;
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

fn prompt_language(explicit: Option<&str>) -> &'static str {
    let value = explicit
        .map(str::to_owned)
        .or_else(|| std::env::var("LC_ALL").ok())
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_default();
    if value.to_ascii_lowercase().starts_with("zh") {
        "zh"
    } else {
        "en"
    }
}

#[cfg(target_os = "windows")]
#[allow(clippy::too_many_lines)]
fn platform_prompt(
    target: &str,
    command_sha256: &str,
    language: &str,
) -> anyhow::Result<PlatformResponse> {
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
$form.ClientSize = New-Object Drawing.Size(520, 330)
$form.Font = New-Object Drawing.Font('Segoe UI', 10)
$form.BackColor = [Drawing.Color]::White
$label = New-Object Windows.Forms.Label
$label.Location = New-Object Drawing.Point(28, 24)
$label.Size = New-Object Drawing.Size(464, 32)
$label.Font = New-Object Drawing.Font('Segoe UI Semibold', 16)
$label.Text = if ($inputJson.language -eq 'zh') { '需要 SSH 凭据' } else { 'SSH credential required' }
$form.Controls.Add($label)
$description = New-Object Windows.Forms.Label
$description.Location = New-Object Drawing.Point(28, 62)
$description.Size = New-Object Drawing.Size(464, 24)
$description.ForeColor = [Drawing.Color]::FromArgb(90, 90, 90)
$description.Text = if ($inputJson.language -eq 'zh') { '输入密码以建立安全连接' } else { 'Enter your password to connect securely' }
$form.Controls.Add($description)
$targetLabel = New-Object Windows.Forms.Label
$targetLabel.Location = New-Object Drawing.Point(28, 102)
$targetLabel.Size = New-Object Drawing.Size(464, 25)
$targetLabel.Font = New-Object Drawing.Font('Consolas', 11)
$targetLabel.Text = $inputJson.target
$form.Controls.Add($targetLabel)
$fingerprint = New-Object Windows.Forms.Label
$fingerprint.Location = New-Object Drawing.Point(28, 128)
$fingerprint.Size = New-Object Drawing.Size(464, 25)
$fingerprint.ForeColor = [Drawing.Color]::FromArgb(90, 90, 90)
$fingerprint.Font = New-Object Drawing.Font('Consolas', 9)
$fingerprint.Text = if ($inputJson.language -eq 'zh') { "命令指纹  $($inputJson.command_sha256.Substring(0, 16))..." } else { "Command fingerprint  $($inputJson.command_sha256.Substring(0, 16))..." }
$form.Controls.Add($fingerprint)
$password = New-Object Windows.Forms.TextBox
$password.Location = New-Object Drawing.Point(28, 176)
$password.Size = New-Object Drawing.Size(464, 30)
$password.Font = New-Object Drawing.Font('Segoe UI', 11)
$password.UseSystemPasswordChar = $true
$form.Controls.Add($password)
$remember = New-Object Windows.Forms.CheckBox
$remember.Location = New-Object Drawing.Point(28, 220)
$remember.Size = New-Object Drawing.Size(464, 26)
$remember.Text = if ($inputJson.language -eq 'zh') { '记住 10 分钟（仅保存在内存中）' } else { 'Remember for 10 minutes (stored in memory only)' }
$remember.Checked = $false
$form.Controls.Add($remember)
$ok = New-Object Windows.Forms.Button
$ok.Location = New-Object Drawing.Point(390, 278)
$ok.Size = New-Object Drawing.Size(102, 34)
$ok.Text = if ($inputJson.language -eq 'zh') { '仅本次使用' } else { 'Use once' }
$ok.BackColor = [Drawing.Color]::FromArgb(30, 110, 230)
$ok.ForeColor = [Drawing.Color]::White
$ok.DialogResult = [Windows.Forms.DialogResult]::OK
$form.AcceptButton = $ok
$form.Controls.Add($ok)
$cancel = New-Object Windows.Forms.Button
$cancel.Location = New-Object Drawing.Point(278, 278)
$cancel.Size = New-Object Drawing.Size(102, 34)
$cancel.Text = if ($inputJson.language -eq 'zh') { '取消' } else { 'Cancel' }
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
        "language": language,
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
fn platform_prompt(
    target: &str,
    command_sha256: &str,
    language: &str,
) -> anyhow::Result<PlatformResponse> {
    const SCRIPT: &str = r"
ObjC.import('Cocoa')

function run(argv) {
  const targetName = argv[0]
  const commandHash = argv[1]
  const chinese = argv[2] === 'zh'
  const alert = $.NSAlert.alloc.init
  const title = chinese ? '需要 SSH 凭据' : 'SSH credential required'
  const description = chinese ? '输入密码以建立安全连接' : 'Enter your password to connect securely'
  const fingerprint = chinese ? '命令指纹  ' : 'Command fingerprint  '
  const placeholder = chinese ? '输入 SSH 密码' : 'Enter SSH password'
  const rememberTitle = chinese ? '记住 10 分钟（仅保存在内存中）' : 'Remember for 10 minutes (stored in memory only)'
  const useTitle = chinese ? '仅本次使用' : 'Use once'
  const cancelTitle = chinese ? '取消' : 'Cancel'

  alert.setMessageText($(title))
  alert.setInformativeText($(description + '\n' + targetName + '\n' + fingerprint + commandHash.slice(0, 16) + '…'))
  alert.addButtonWithTitle($(useTitle))
  alert.addButtonWithTitle($(cancelTitle))

  const accessory = $.NSView.alloc.initWithFrame($.NSMakeRect(0, 0, 360, 72))
  const password = $.NSSecureTextField.alloc.initWithFrame($.NSMakeRect(0, 40, 360, 28))
  password.setPlaceholderString($(placeholder))
  const remember = $.NSButton.alloc.initWithFrame($.NSMakeRect(0, 8, 360, 24))
  remember.setButtonType($.NSSwitchButton)
  remember.setTitle($(rememberTitle))
  accessory.addSubview(password)
  accessory.addSubview(remember)
  alert.setAccessoryView(accessory)
  alert.window.setInitialFirstResponder(password)

  const result = alert.runModal
  if (result == $.NSAlertSecondButtonReturn) return 'cancel\n'
  const action = remember.state == $.NSControlStateValueOn ? 'remember_10_minutes' : 'use_once'
  const secret = password.stringValue.js
  password.setStringValue($(''))
  return action + '\n' + secret
}
";
    let output = Command::new("osascript")
        .args([
            "-l",
            "JavaScript",
            "-e",
            SCRIPT,
            "--",
            target,
            command_sha256,
            language,
        ])
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
fn platform_prompt(
    _target: &str,
    _command_sha256: &str,
    _language: &str,
) -> anyhow::Result<PlatformResponse> {
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
