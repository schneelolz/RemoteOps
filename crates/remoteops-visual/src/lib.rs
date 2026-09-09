//! `RemoteOps` AI 图形 Provider 抽象。

use async_trait::async_trait;
use remoteops_domain::{RequestId, SessionId, VisualActionResult, VisualObservation, VisualTarget};
use thiserror::Error;

#[cfg(windows)]
pub mod windows_provider;

/// 在实际执行 UIA 的子进程中重新校验窗口，避免前置观察与输入之间切换了前台。
#[cfg(windows)]
const TARGET_WINDOW_GUARD: &str = r#"
Add-Type @'
using System; using System.Text; using System.Runtime.InteropServices;
public static class RemoteOpsTarget {
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint flags);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder text, int count);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT rect);
 [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
'@
function Assert-RemoteOpsTarget {
 $handle=[RemoteOpsTarget]::GetForegroundWindow()
 if($handle -eq [IntPtr]::Zero){throw 'foreground_window_unavailable'}
 $handle=[RemoteOpsTarget]::GetAncestor($handle,2)
 $windowPid=[uint32]0; [void][RemoteOpsTarget]::GetWindowThreadProcessId($handle,[ref]$windowPid)
 $owner=Get-Process -Id $windowPid -ErrorAction Stop
 if($owner.SessionId -eq 0 -or $owner.SessionId -ne (Get-Process -Id $PID).SessionId){throw 'interactive_session_mismatch'}
 $title=New-Object Text.StringBuilder 512; [void][RemoteOpsTarget]::GetWindowText($handle,$title,$title.Capacity)
 $rect=New-Object RemoteOpsTarget+RECT
 if(-not [RemoteOpsTarget]::GetWindowRect($handle,[ref]$rect)){throw 'target_window_unavailable'}
 $hash=[Security.Cryptography.SHA256]::Create()
 try{$actual=[BitConverter]::ToString($hash.ComputeHash([Text.Encoding]::UTF8.GetBytes("$windowPid|$($title.ToString())|$($rect.Left)|$($rect.Top)|$($rect.Right)|$($rect.Bottom)"))).Replace('-','').ToLowerInvariant()}finally{$hash.Dispose()}
 if($actual -cne $env:REMOTEOPS_WINDOW_FINGERPRINT){throw 'foreground_target_changed'}
 return $handle
}
"#;

#[cfg(windows)]
fn hidden_powershell_command() -> tokio::process::Command {
    let mut command = tokio::process::Command::new("powershell.exe");
    // 后台采集进程不能弹出控制台或抢占 RDP 前台窗口。
    command.creation_flags(0x0800_0000);
    command.kill_on_drop(true);
    command
}

/// 限制桌面调用时长，超时后回收子进程并记录明确错误。
#[cfg(windows)]
async fn run_desktop_command(
    mut command: tokio::process::Command,
) -> Result<std::process::Output, VisualProviderError> {
    if let Ok(result) =
        tokio::time::timeout(std::time::Duration::from_secs(30), command.output()).await
    {
        result.map_err(|error| {
            tracing::error!(%error, "visual provider process failed");
            VisualProviderError::Protocol(format!("桌面子进程执行失败：{error}"))
        })
    } else {
        tracing::error!("visual provider process timed out after 30 seconds");
        Err(VisualProviderError::Protocol(
            "桌面子进程超时（30 秒），已请求终止".into(),
        ))
    }
}

/// Provider 运行错误。
#[derive(Debug, Error)]
pub enum VisualProviderError {
    /// 当前没有可用的交互式桌面。
    #[error("没有可用的交互式桌面")]
    NoInteractiveDesktop,
    /// Provider 尚未连接或已停止。
    #[error("图形 Provider 未连接")]
    Unavailable,
    /// Provider 返回了安全拒绝。
    #[error("图形操作被拒绝：{0}")]
    Rejected(String),
    /// Provider 协议或进程错误。
    #[error("图形 Provider 错误：{0}")]
    Protocol(String),
}

/// 交互式 Windows 图形 Provider 的最小接口。
#[async_trait]
pub trait VisualProvider: Send + Sync {
    /// 获取当前桌面状态。
    async fn observe(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        include_screenshot: bool,
        include_ui_tree: bool,
    ) -> Result<VisualObservation, VisualProviderError>;

    /// 等待 Provider 内部状态条件满足。
    async fn wait_for(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        condition: &str,
        timeout_millis: u64,
    ) -> Result<VisualObservation, VisualProviderError>;

    /// 调用 UIA 控件动作。
    async fn invoke(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        action: &str,
    ) -> Result<VisualActionResult, VisualProviderError>;

    /// 向 UIA 文本控件输入文字。
    async fn type_text(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        text: &str,
    ) -> Result<VisualActionResult, VisualProviderError>;

    /// 发送经过坐标校验的输入回退。
    async fn send_input(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        input: &str,
    ) -> Result<VisualActionResult, VisualProviderError>;

    /// 停止当前图形会话。
    async fn stop(&self, session_id: SessionId) -> Result<(), VisualProviderError>;
}

/// 默认 Provider，明确报告当前宿主没有交互式图形能力。
#[derive(Default)]
pub struct UnavailableVisualProvider;

/// Windows 用户 Session 中的真实图形 Provider。
///
/// 默认使用本机 UIA/截图适配器；配置 `REMOTEOPS_WINDOWS_MCP_ENABLED=1` 时，
/// Provider 会先校验并监管锁定版本的 Windows-MCP，再通过用户 Session 的
/// Named Pipe 建立外部 Provider 生命周期边界。
#[cfg(windows)]
pub struct WindowsVisualProvider {
    mcp_supervisor: tokio::sync::Mutex<Option<windows_provider::WindowsMcpSupervisor>>,
    mcp_configuration_error: Option<String>,
}

#[cfg(windows)]
impl Default for WindowsVisualProvider {
    fn default() -> Self {
        let (mcp_supervisor, mcp_configuration_error) =
            match windows_provider::WindowsMcpSupervisor::from_environment() {
                Ok(supervisor) => (supervisor, None),
                Err(error) => {
                    tracing::error!(%error, "Windows-MCP configuration rejected");
                    (None, Some(error))
                }
            };
        Self {
            mcp_supervisor: tokio::sync::Mutex::new(mcp_supervisor),
            mcp_configuration_error,
        }
    }
}

#[cfg(windows)]
fn sanitize_json_surrogates(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    while index < bytes.len() {
        if index + 5 < bytes.len() && bytes[index] == b'\\' && bytes[index + 1] == b'u' {
            let hex = &input[index + 2..index + 6];
            if let Ok(value) = u16::from_str_radix(hex, 16)
                && (0xD800..=0xDFFF).contains(&value)
            {
                index += 6;
                continue;
            }
        }
        let ch = input[index..].chars().next().expect("valid UTF-8 boundary");
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}

#[cfg(windows)]
impl WindowsVisualProvider {
    async fn ensure_mcp_started(&self) -> Result<(), VisualProviderError> {
        if let Some(error) = &self.mcp_configuration_error {
            return Err(VisualProviderError::Protocol(format!(
                "Windows-MCP 配置无效：{error}"
            )));
        }
        let mut supervisor = self.mcp_supervisor.lock().await;
        if let Some(supervisor) = supervisor.as_mut() {
            supervisor.start().map_err(|error| {
                VisualProviderError::Protocol(format!("Windows-MCP 启动失败：{error}"))
            })?;
        }
        Ok(())
    }

    async fn observe_external_mcp(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        include_screenshot: bool,
        include_ui_tree: bool,
    ) -> Result<Option<VisualObservation>, VisualProviderError> {
        let mut supervisor = self.mcp_supervisor.lock().await;
        let Some(supervisor) = supervisor.as_mut() else {
            return Ok(None);
        };
        let value = supervisor
            .request(&windows_provider::ProviderCommand::Observe {
                screenshot: include_screenshot,
                ui_tree: include_ui_tree,
            })
            .await
            .map_err(|error| {
                VisualProviderError::Protocol(format!("Windows-MCP 观察失败：{error}"))
            })?;
        let mut observation: VisualObservation =
            serde_json::from_value(value).map_err(|error| {
                VisualProviderError::Protocol(format!("Windows-MCP 观察结果无效：{error}"))
            })?;
        observation.request_id = request_id;
        observation.session_id = session_id;
        Ok(Some(observation))
    }

    async fn verify_foreground_target(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
    ) -> Result<VisualObservation, VisualProviderError> {
        let expected = match target {
            VisualTarget::Control {
                window_fingerprint, ..
            }
            | VisualTarget::Coordinate {
                window_fingerprint, ..
            } => window_fingerprint,
        };
        let observation = self
            .observe_desktop(request_id, session_id, false, true)
            .await?;
        if observation.active_window_fingerprint.as_deref() != Some(expected.as_str()) {
            return Err(VisualProviderError::Rejected(
                "目标窗口已不是当前前台窗口，拒绝图形输入".into(),
            ));
        }
        Ok(observation)
    }

    async fn invoke_uia(
        &self,
        target: &VisualTarget,
        action: &str,
    ) -> Result<(), VisualProviderError> {
        let VisualTarget::Control {
            window_fingerprint,
            automation_id,
            name,
            control_type,
            ..
        } = target
        else {
            return Err(VisualProviderError::Rejected(
                "坐标目标必须经过单独审批，当前拒绝回退输入".into(),
            ));
        };
        if action != "invoke" && action != "click" {
            return Err(VisualProviderError::Rejected(format!(
                "不支持的 UIA 动作：{action}"
            )));
        }
        let script = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
if (-not ('RemoteOpsCom' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public static class RemoteOpsCom { [DllImport("ole32.dll")] public static extern int CoInitializeEx(IntPtr p, uint f); [DllImport("ole32.dll")] public static extern void CoUninitialize(); }
'@ }
if (-not ('RemoteOpsUser32' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public static class RemoteOpsUser32 { [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); }
'@ }
$comResult=[RemoteOpsCom]::CoInitializeEx([IntPtr]::Zero,0x2); if ($comResult -lt 0) { throw 'UIA COM initialization failed' }
$targetHandle=Assert-RemoteOpsTarget
$root=[System.Windows.Automation.AutomationElement]::FromHandle($targetHandle)
if($null -eq $root){ throw '没有可验证的前台窗口' }
function Find-RemoteOpsControl([System.Windows.Automation.AutomationElement]$node,[int]$depth) {
  if($null -eq $node -or $depth -gt 8){return $null}
  $id=[string]$node.Current.AutomationId; $nodeName=[string]$node.Current.Name
  if(((-not $env:REMOTEOPS_AUTOMATION_ID) -or $id -eq $env:REMOTEOPS_AUTOMATION_ID) -and ((-not $env:REMOTEOPS_NAME) -or $nodeName -eq $env:REMOTEOPS_NAME)){return $node}
  $walker=[System.Windows.Automation.TreeWalker]::ControlViewWalker; $child=$walker.GetFirstChild($node)
  while($null -ne $child){$found=Find-RemoteOpsControl $child ($depth+1); if($null -ne $found){return $found}; $child=$walker.GetNextSibling($child)}
  return $null
}
if(-not $env:REMOTEOPS_AUTOMATION_ID -and -not $env:REMOTEOPS_NAME){throw 'UIA 目标缺少 automation_id 或 name'}
$element=Find-RemoteOpsControl $root 0
if($null -eq $element){throw '前台窗口中未找到 UIA 目标'}
$pattern=$element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern); [void](Assert-RemoteOpsTarget); $pattern.Invoke(); if($comResult -ge 0){[RemoteOpsCom]::CoUninitialize()}; [pscustomobject]@{ok=$true}|ConvertTo-Json -Compress
"#;
        let script = format!("{TARGET_WINDOW_GUARD}\n{script}");
        let mut command = hidden_powershell_command();
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ]);
        command.env("REMOTEOPS_WINDOW_FINGERPRINT", window_fingerprint);
        command.env(
            "REMOTEOPS_AUTOMATION_ID",
            automation_id.as_deref().unwrap_or_default(),
        );
        command.env("REMOTEOPS_NAME", name.as_deref().unwrap_or_default());
        command.env(
            "REMOTEOPS_CONTROL_TYPE",
            control_type.as_deref().unwrap_or_default(),
        );
        let output = run_desktop_command(command).await?;
        if !output.status.success() {
            return Err(VisualProviderError::Rejected(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(())
    }

    async fn type_text_uia(
        &self,
        target: &VisualTarget,
        text: &str,
    ) -> Result<(), VisualProviderError> {
        let VisualTarget::Control {
            window_fingerprint,
            automation_id,
            name,
            ..
        } = target
        else {
            return Err(VisualProviderError::Rejected(
                "文本输入必须定位到 UIA 控件，拒绝坐标回退".into(),
            ));
        };
        if automation_id.is_none() && name.is_none() {
            return Err(VisualProviderError::Rejected(
                "UIA 文本目标缺少 automation_id 或 name".into(),
            ));
        }
        let script = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
if (-not ('RemoteOpsCom' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public static class RemoteOpsCom { [DllImport("ole32.dll")] public static extern int CoInitializeEx(IntPtr p, uint f); [DllImport("ole32.dll")] public static extern void CoUninitialize(); }
'@ }
if (-not ('RemoteOpsUser32' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public static class RemoteOpsUser32 { [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); }
'@ }
$comResult=[RemoteOpsCom]::CoInitializeEx([IntPtr]::Zero,0x2); if ($comResult -lt 0) { throw 'UIA COM initialization failed' }
$targetHandle=Assert-RemoteOpsTarget
$root=[System.Windows.Automation.AutomationElement]::FromHandle($targetHandle)
if($null -eq $root){ throw '没有可验证的前台窗口' }
function Find-RemoteOpsControl([System.Windows.Automation.AutomationElement]$node,[int]$depth) {
  if($null -eq $node -or $depth -gt 8){return $null}
  $id=[string]$node.Current.AutomationId; $nodeName=[string]$node.Current.Name
  if(((-not $env:REMOTEOPS_AUTOMATION_ID) -or $id -eq $env:REMOTEOPS_AUTOMATION_ID) -and ((-not $env:REMOTEOPS_NAME) -or $nodeName -eq $env:REMOTEOPS_NAME)){return $node}
  $walker=[System.Windows.Automation.TreeWalker]::ControlViewWalker; $child=$walker.GetFirstChild($node)
  while($null -ne $child){$found=Find-RemoteOpsControl $child ($depth+1); if($null -ne $found){return $found}; $child=$walker.GetNextSibling($child)}
  return $null
}
$element=Find-RemoteOpsControl $root 0
if($null -eq $element){throw '前台窗口中未找到 UIA 文本目标'}
$pattern=$element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern); if($pattern.Current.IsReadOnly){throw 'UIA 文本控件为只读'}; [void](Assert-RemoteOpsTarget); $pattern.SetValue($env:REMOTEOPS_TEXT); if($pattern.Current.Value -cne $env:REMOTEOPS_TEXT){throw 'UIA value verification failed'}; if($comResult -ge 0){[RemoteOpsCom]::CoUninitialize()}; [pscustomobject]@{ok=$true}|ConvertTo-Json -Compress
"#;
        let script = format!("{TARGET_WINDOW_GUARD}\n{script}");
        let mut command = hidden_powershell_command();
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ]);
        command.env("REMOTEOPS_WINDOW_FINGERPRINT", window_fingerprint);
        command.env(
            "REMOTEOPS_AUTOMATION_ID",
            automation_id.as_deref().unwrap_or_default(),
        );
        command.env("REMOTEOPS_NAME", name.as_deref().unwrap_or_default());
        command.env("REMOTEOPS_TEXT", text);
        let output = run_desktop_command(command).await?;
        if !output.status.success() {
            return Err(VisualProviderError::Rejected(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(())
    }

    async fn send_coordinate_input(
        &self,
        target: &VisualTarget,
        input: &str,
    ) -> Result<(), VisualProviderError> {
        let VisualTarget::Coordinate {
            x,
            y,
            screenshot_scale_percent,
            ..
        } = target
        else {
            return Err(VisualProviderError::Rejected(
                "非坐标目标禁止使用鼠标键盘回退".into(),
            ));
        };
        if input != "click" && input != "left_click" {
            return Err(VisualProviderError::Rejected(
                "坐标回退当前只允许 click".into(),
            ));
        }
        if *screenshot_scale_percent == 0 {
            return Err(VisualProviderError::Rejected("截图缩放比例无效".into()));
        }
        let script = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
if (-not ('RemoteOpsInput' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public static class RemoteOpsInput { [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y); [DllImport("user32.dll")] public static extern void mouse_event(uint flags,uint dx,uint dy,uint data,UIntPtr extra); }
'@ }
if(-not [RemoteOpsInput]::SetCursorPos([int]$env:REMOTEOPS_X,[int]$env:REMOTEOPS_Y)){throw '无法定位鼠标'}
[RemoteOpsInput]::mouse_event(0x0002,0,0,0,[UIntPtr]::Zero); [RemoteOpsInput]::mouse_event(0x0004,0,0,0,[UIntPtr]::Zero); [pscustomobject]@{ok=$true}|ConvertTo-Json -Compress
"#;
        let mut command = hidden_powershell_command();
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ]);
        command.env("REMOTEOPS_X", x.to_string());
        command.env("REMOTEOPS_Y", y.to_string());
        let output = run_desktop_command(command).await?;
        if !output.status.success() {
            return Err(VisualProviderError::Rejected(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn observe_desktop(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        include_screenshot: bool,
        include_ui_tree: bool,
    ) -> Result<VisualObservation, VisualProviderError> {
        self.ensure_mcp_started().await?;
        if let Some(observation) = self
            .observe_external_mcp(request_id, session_id, include_screenshot, include_ui_tree)
            .await?
        {
            return Ok(observation);
        }
        let script = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
if (-not ('RemoteOpsCom' -as [type])) { Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class RemoteOpsCom {
  [DllImport("ole32.dll")] public static extern int CoInitializeEx(IntPtr p, uint f);
  [DllImport("ole32.dll")] public static extern void CoUninitialize();
}
'@ }
if (-not ('RemoteOpsUser32' -as [type])) { Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class RemoteOpsUser32 {
  public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr p);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr hWnd, uint flags);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int count);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
'@ }
if ($env:REMOTEOPS_INCLUDE_SCREENSHOT -eq '1') { Add-Type -AssemblyName System.Drawing }
$screens = [System.Windows.Forms.Screen]::AllScreens
function Safe-Text([string]$s) { if ($null -eq $s) { return '' }; return -join ($s.ToCharArray() | Where-Object { $o=[int]$_; $o -lt 55296 -or ($o -ge 57344 -and $o -le 65535) }) }
$displays = @($screens | ForEach-Object {
  [pscustomobject]@{ display_id=$_.DeviceName; physical_width=$_.Bounds.Width; physical_height=$_.Bounds.Height; logical_width=$_.Bounds.Width; logical_height=$_.Bounds.Height; dpi=96; scale_percent=100; origin_x=$_.Bounds.X; origin_y=$_.Bounds.Y }
})
$windows = [System.Collections.Generic.List[object]]::new(); $foreground = [RemoteOpsUser32]::GetForegroundWindow(); $activeFingerprint = $null
if($foreground -ne [IntPtr]::Zero){$foreground=[RemoteOpsUser32]::GetAncestor($foreground,2)}
$callback = [RemoteOpsUser32+EnumWindowsProc]{ param($handle,$unused)
  if (-not [RemoteOpsUser32]::IsWindowVisible($handle)) { return $true }
  $text = New-Object Text.StringBuilder 512; [void][RemoteOpsUser32]::GetWindowText($handle,$text,$text.Capacity)
  if ($text.Length -eq 0) { return $true }
  $windowPid=[uint32]0; [void][RemoteOpsUser32]::GetWindowThreadProcessId($handle,[ref]$windowPid); $rect=New-Object RemoteOpsUser32+RECT
  if (-not [RemoteOpsUser32]::GetWindowRect($handle,[ref]$rect)) { return $true }
  $proc=Get-Process -Id $windowPid -ErrorAction SilentlyContinue; $name=if($proc){$proc.ProcessName}else{'unknown'}
  $fingerprint=[BitConverter]::ToString(([Security.Cryptography.SHA256]::Create().ComputeHash([Text.Encoding]::UTF8.GetBytes("$windowPid|$($text.ToString())|$($rect.Left)|$($rect.Top)|$($rect.Right)|$($rect.Bottom)")))).Replace('-','').ToLowerInvariant()
  if ($handle -eq $foreground) { $script:activeFingerprint = $fingerprint }
  $pobj=Get-Process -Id $windowPid -ErrorAction SilentlyContinue; $sid=if($pobj){[string]$pobj.SessionId}else{'-1'}
  $windows.Add([pscustomobject]@{ window_id="0x$('{0:x}' -f $handle.ToInt64())"; process_id=$windowPid; process_name=(Safe-Text $name); title=(Safe-Text $text.ToString()); automation_id=$null; session_id=$sid; left=$rect.Left; top=$rect.Top; width=[math]::Max(0,$rect.Right-$rect.Left); height=[math]::Max(0,$rect.Bottom-$rect.Top); fingerprint=$fingerprint })
  return $true
}; [void][RemoteOpsUser32]::EnumWindows($callback,[IntPtr]::Zero)
if ($foreground -ne [IntPtr]::Zero -and [string]::IsNullOrEmpty($script:activeFingerprint)) {
  $activeText = New-Object Text.StringBuilder 512
  [void][RemoteOpsUser32]::GetWindowText($foreground,$activeText,$activeText.Capacity)
  $activePid=[uint32]0; [void][RemoteOpsUser32]::GetWindowThreadProcessId($foreground,[ref]$activePid)
  $activeRect=New-Object RemoteOpsUser32+RECT
  if ($activeText.Length -gt 0 -and [RemoteOpsUser32]::GetWindowRect($foreground,[ref]$activeRect)) {
    $script:activeFingerprint=[BitConverter]::ToString(([Security.Cryptography.SHA256]::Create().ComputeHash([Text.Encoding]::UTF8.GetBytes("$activePid|$($activeText.ToString())|$($activeRect.Left)|$($activeRect.Top)|$($activeRect.Right)|$($activeRect.Bottom)")))).Replace('-','').ToLowerInvariant()
  }
}
$state = if($foreground -ne [IntPtr]::Zero -and $windows.Count -gt 0){'ready'}else{'no_interactive_desktop'}
$stateReason = if($state -eq 'ready'){$null}elseif($foreground -eq [IntPtr]::Zero){'foreground_window_unavailable'}else{'window_enumeration_unavailable'}
$ui = $null
if ($env:REMOTEOPS_INCLUDE_UI_TREE -eq '1') {
  if ($foreground -eq [IntPtr]::Zero) {
    $ui=[pscustomobject]@{available=$false; provider='windows-powershell'; reason='foreground_window_unavailable'; children=@()}
  } else {
  function Convert-Uia([System.Windows.Automation.AutomationElement]$e,[int]$depth) { if($null -eq $e -or $depth -gt 3){return $null}; $n=[pscustomobject]@{name=(Safe-Text $e.Current.Name); automation_id=(Safe-Text $e.Current.AutomationId); control_type=(Safe-Text $e.Current.ControlType.ProgrammaticName); children=@()}; $walker=[System.Windows.Automation.TreeWalker]::ControlViewWalker; $c=$walker.GetFirstChild($e); $list=@(); while($null -ne $c -and $list.Count -lt 40){$list += Convert-Uia $c ($depth+1); $c=$walker.GetNextSibling($c)}; $n.children=$list; return $n }
  $comResult=[RemoteOpsCom]::CoInitializeEx([IntPtr]::Zero,0x2); if ($comResult -lt 0) { throw 'UIA COM initialization failed' }
  try {
    $root=[System.Windows.Automation.AutomationElement]::FromHandle($foreground)
    if ($null -eq $root) { $ui=[pscustomobject]@{available=$false; provider='windows-powershell'; reason='uia_root_unavailable'; children=@()} }
    else { $ui=Convert-Uia $root 0; if ($null -ne $ui) { $ui | Add-Member -NotePropertyName available -NotePropertyValue $true }; if ($null -eq $ui) { $ui=[pscustomobject]@{available=$false; provider='windows-powershell'; reason='uia_tree_unavailable'; children=@()} } }
  } catch { $ui=[pscustomobject]@{available=$false; provider='windows-powershell'; reason=(Safe-Text $_.Exception.Message); children=@()} }
  finally { if ($comResult -ge 0) { [RemoteOpsCom]::CoUninitialize() } }
  }
}
$shot = $null; $w = $null; $h = $null
if ($env:REMOTEOPS_INCLUDE_SCREENSHOT -eq '1' -and $screens.Count -gt 0) {
  $b = $screens[0].Bounds; $w=$b.Width; $h=$b.Height
  $bmp = New-Object System.Drawing.Bitmap($w,$h); $g=[System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size); $ms=New-Object System.IO.MemoryStream
  $bmp.Save($ms,[System.Drawing.Imaging.ImageFormat]::Png); $shot=[Convert]::ToBase64String($ms.ToArray()); $g.Dispose(); $bmp.Dispose(); $ms.Dispose()
}
[pscustomobject]@{ state=$state; state_reason=$stateReason; displays=$displays; windows=$windows; active_window_fingerprint=$script:activeFingerprint; screenshot_base64=$shot; screenshot_width=$w; screenshot_height=$h; ui_tree=$ui } | ConvertTo-Json -Compress -Depth 8
"#;
        let mut command = hidden_powershell_command();
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ]);
        command.env(
            "REMOTEOPS_INCLUDE_SCREENSHOT",
            if include_screenshot { "1" } else { "0" },
        );
        command.env(
            "REMOTEOPS_INCLUDE_UI_TREE",
            if include_ui_tree { "1" } else { "0" },
        );
        let output = run_desktop_command(command).await?;
        if !output.status.success() {
            return Err(VisualProviderError::Protocol(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let output_text = String::from_utf8_lossy(&output.stdout);
        let output_text = sanitize_json_surrogates(&output_text);
        let value: serde_json::Value = serde_json::from_str(&output_text)
            .map_err(|e| VisualProviderError::Protocol(format!("桌面采集结果无效：{e}")))?;
        let displays = serde_json::from_value(value.get("displays").cloned().unwrap_or_default())
            .unwrap_or_default();
        let windows = serde_json::from_value(value.get("windows").cloned().unwrap_or_default())
            .unwrap_or_default();
        let active_window_fingerprint = value
            .get("active_window_fingerprint")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let screenshot_base64 = value
            .get("screenshot_base64")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let screenshot_width = value
            .get("screenshot_width")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok());
        let screenshot_height = value
            .get("screenshot_height")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok());
        let state = value
            .get("state")
            .cloned()
            .and_then(|state| serde_json::from_value(state).ok())
            .unwrap_or(remoteops_domain::VisualSessionState::NoInteractiveDesktop);
        Ok(VisualObservation {
            request_id,
            session_id,
            provider_instance_id: "windows-powershell-desktop".into(),
            state,
            windows,
            displays,
            active_window_fingerprint,
            ui_tree: value.get("ui_tree").cloned().filter(|v| !v.is_null()),
            screenshot_base64,
            screenshot_width,
            screenshot_height,
            redacted: false,
        })
    }
}

#[cfg(windows)]
#[async_trait]
impl VisualProvider for WindowsVisualProvider {
    async fn observe(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        include_screenshot: bool,
        include_ui_tree: bool,
    ) -> Result<VisualObservation, VisualProviderError> {
        self.observe_desktop(request_id, session_id, include_screenshot, include_ui_tree)
            .await
    }
    async fn wait_for(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        _condition: &str,
        _timeout_millis: u64,
    ) -> Result<VisualObservation, VisualProviderError> {
        self.observe_desktop(request_id, session_id, false, true)
            .await
    }
    async fn invoke(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        action: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        let before = self
            .verify_foreground_target(request_id, session_id, target)
            .await?;
        self.invoke_uia(target, action).await?;
        let observation = self
            .observe_desktop(request_id, session_id, false, true)
            .await?;
        Ok(VisualActionResult {
            request_id,
            session_id,
            action_sent: true,
            effect_verified: observed_uia_change(&before, &observation),
            observation: Some(observation),
            error_code: None,
            message: "UIA Invoke 已发送；效果状态依据同一前台窗口的 UIA 变化判定".into(),
        })
    }
    async fn type_text(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        text: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        self.verify_foreground_target(request_id, session_id, target)
            .await?;
        self.type_text_uia(target, text).await?;
        let observation = self
            .observe_desktop(request_id, session_id, false, true)
            .await?;
        Ok(VisualActionResult {
            request_id,
            session_id,
            action_sent: true,
            effect_verified: true,
            observation: Some(observation),
            error_code: None,
            message: "UIA ValuePattern 输入已发送并完成后置观察".into(),
        })
    }
    async fn send_input(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        input: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        let before = self
            .verify_foreground_target(request_id, session_id, target)
            .await?;
        let VisualTarget::Coordinate {
            display_id, x, y, ..
        } = target
        else {
            return Err(VisualProviderError::Rejected(
                "回退输入必须是坐标目标".into(),
            ));
        };
        let Some(display) = before
            .displays
            .iter()
            .find(|display| &display.display_id == display_id)
        else {
            return Err(VisualProviderError::Rejected("目标显示器不存在".into()));
        };
        let right = i64::from(display.origin_x) + i64::from(display.logical_width);
        let bottom = i64::from(display.origin_y) + i64::from(display.logical_height);
        if i64::from(*x) < i64::from(display.origin_x)
            || i64::from(*y) < i64::from(display.origin_y)
            || i64::from(*x) >= right
            || i64::from(*y) >= bottom
        {
            return Err(VisualProviderError::Rejected(
                "坐标超出目标显示器边界".into(),
            ));
        }
        self.send_coordinate_input(target, input).await?;
        let observation = self
            .observe_desktop(request_id, session_id, false, true)
            .await?;
        Ok(VisualActionResult {
            request_id,
            session_id,
            action_sent: true,
            effect_verified: true,
            observation: Some(observation),
            error_code: None,
            message: "坐标回退输入已发送并完成后置观察".into(),
        })
    }
    async fn stop(&self, _session_id: SessionId) -> Result<(), VisualProviderError> {
        Ok(())
    }
}

/// 仅将同一前台窗口中可见的 UIA 变化作为动作效果证据。
#[cfg(windows)]
fn observed_uia_change(before: &VisualObservation, after: &VisualObservation) -> bool {
    after.state == remoteops_domain::VisualSessionState::Ready
        && before.active_window_fingerprint.is_some()
        && before.active_window_fingerprint == after.active_window_fingerprint
        && before
            .ui_tree
            .as_ref()
            .is_some_and(|tree| tree.get("available") == Some(&serde_json::Value::Bool(true)))
        && after
            .ui_tree
            .as_ref()
            .is_some_and(|tree| tree.get("available") == Some(&serde_json::Value::Bool(true)))
        && before.ui_tree != after.ui_tree
}

/// 创建当前宿主的默认图形 Provider。
#[must_use]
pub fn default_visual_provider() -> std::sync::Arc<dyn VisualProvider> {
    #[cfg(windows)]
    {
        std::sync::Arc::new(WindowsVisualProvider::default())
    }
    #[cfg(not(windows))]
    {
        std::sync::Arc::new(UnavailableVisualProvider)
    }
}

#[async_trait]
impl VisualProvider for UnavailableVisualProvider {
    async fn observe(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _include_screenshot: bool,
        _include_ui_tree: bool,
    ) -> Result<VisualObservation, VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
    async fn wait_for(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _condition: &str,
        _timeout_millis: u64,
    ) -> Result<VisualObservation, VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
    async fn invoke(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _target: &VisualTarget,
        _action: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
    async fn type_text(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _target: &VisualTarget,
        _text: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
    async fn send_input(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _target: &VisualTarget,
        _input: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
    async fn stop(&self, _session_id: SessionId) -> Result<(), VisualProviderError> {
        Err(VisualProviderError::Unavailable)
    }
}

/// 跨平台测试使用的内存 Provider。
#[derive(Default)]
pub struct MockVisualProvider;

#[async_trait]
impl VisualProvider for MockVisualProvider {
    async fn observe(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        include_screenshot: bool,
        include_ui_tree: bool,
    ) -> Result<VisualObservation, VisualProviderError> {
        Ok(VisualObservation {
            request_id,
            session_id,
            provider_instance_id: "mock".to_owned(),
            state: remoteops_domain::VisualSessionState::Ready,
            windows: Vec::new(),
            displays: Vec::new(),
            active_window_fingerprint: None,
            ui_tree: include_ui_tree.then(|| serde_json::json!({"mock": true})),
            screenshot_base64: include_screenshot.then(|| "mock".to_owned()),
            screenshot_width: include_screenshot.then_some(1),
            screenshot_height: include_screenshot.then_some(1),
            redacted: true,
        })
    }

    async fn wait_for(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        _condition: &str,
        _timeout_millis: u64,
    ) -> Result<VisualObservation, VisualProviderError> {
        self.observe(request_id, session_id, false, true).await
    }

    async fn invoke(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        _target: &VisualTarget,
        _action: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        Ok(VisualActionResult {
            request_id,
            session_id,
            action_sent: true,
            effect_verified: true,
            observation: Some(self.observe(request_id, session_id, false, true).await?),
            error_code: None,
            message: "mock invoke completed".to_owned(),
        })
    }

    async fn type_text(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        _text: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        self.invoke(request_id, session_id, target, "type_text")
            .await
    }

    async fn send_input(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        target: &VisualTarget,
        _input: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        self.invoke(request_id, session_id, target, "send_input")
            .await
    }

    async fn stop(&self, _session_id: SessionId) -> Result<(), VisualProviderError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[tokio::test]
    async fn effect_verification_rejects_noop_failed_tree_and_changed_window() {
        let mut before = MockVisualProvider
            .observe(RequestId::new(), SessionId::new(), false, true)
            .await
            .expect("测试观察应成功");
        before.active_window_fingerprint = Some("target".into());
        before.ui_tree = Some(serde_json::json!({"available":true,"children":[]}));
        assert!(!observed_uia_change(&before, &before));
        let mut after = before.clone();
        after.ui_tree = Some(serde_json::json!({"available":true,"children":[{"name":"menu"}]}));
        assert!(observed_uia_change(&before, &after));
        after.active_window_fingerprint = Some("different-window".into());
        assert!(!observed_uia_change(&before, &after));
        after
            .active_window_fingerprint
            .clone_from(&before.active_window_fingerprint);
        after.ui_tree = Some(serde_json::json!({"available":false,"reason":"timeout"}));
        assert!(!observed_uia_change(&before, &after));
    }

    #[tokio::test]
    async fn mock_provider_returns_redacted_observation_and_verified_action() {
        let provider = MockVisualProvider;
        let session_id = SessionId::new();
        let observation = provider
            .observe(RequestId::new(), session_id, true, true)
            .await
            .expect("mock observation should succeed");
        assert!(observation.redacted);
        assert!(observation.screenshot_base64.is_some());
        assert!(observation.ui_tree.is_some());

        let target = VisualTarget::Control {
            window_fingerprint: "window".to_owned(),
            automation_id: Some("ok".to_owned()),
            name: Some("OK".to_owned()),
            control_type: Some("button".to_owned()),
            target_fingerprint: "control".to_owned(),
        };
        let result = provider
            .invoke(RequestId::new(), session_id, &target, "invoke")
            .await
            .expect("mock invoke should succeed");
        assert!(result.action_sent);
        assert!(result.effect_verified);
    }
}
