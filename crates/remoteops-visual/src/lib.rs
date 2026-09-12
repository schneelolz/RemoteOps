//! `RemoteOps` AI 图形 Provider 抽象。

use async_trait::async_trait;
use remoteops_domain::{RequestId, SessionId, VisualActionResult, VisualObservation, VisualTarget};
use thiserror::Error;

#[cfg(windows)]
pub mod windows_provider;

#[cfg(windows)]
pub mod windows_mcp_stdio;

/// 观察和动作共用相同的控件身份算法，区分同名、同类型的不同 UIA 实例。
#[cfg(windows)]
const UIA_CONTROL_HELPERS: &str = r"
function Safe-Text([string]$s) {
 if($null -eq $s){return ''}
 return [Text.Encoding]::UTF8.GetString([Text.Encoding]::UTF8.GetBytes($s))
}
function Get-RemoteOpsControlFingerprint($element,[string]$windowFingerprint) {
 $runtimeId=@($element.GetRuntimeId())
 if($runtimeId.Count -eq 0){throw 'uia_runtime_id_unavailable'}
 $identity=ConvertTo-Json -InputObject @($windowFingerprint,$runtimeId,(Safe-Text $element.Current.AutomationId),(Safe-Text $element.Current.Name),[string]$element.Current.ControlType.ProgrammaticName) -Compress -Depth 4
 $hash=[Security.Cryptography.SHA256]::Create()
 try{return [BitConverter]::ToString($hash.ComputeHash([Text.Encoding]::UTF8.GetBytes($identity))).Replace('-','').ToLowerInvariant()}finally{$hash.Dispose()}
}
function Test-RemoteOpsControlType([string]$actual,[string]$expected) {
 if([string]::IsNullOrEmpty($expected)){return $true}
 if($actual -ceq $expected){return $true}
 # 控制器允许传入 button 这类短名称，观察树使用 UIA 的标准 ControlType.Button。
 return $actual -ceq ('ControlType.' + $expected.Substring(0,1).ToUpperInvariant() + $expected.Substring(1).ToLowerInvariant())
}
function Find-RemoteOpsControl($node,[int]$depth) {
 if($null -eq $node -or $depth -gt 8){return $null}
 if(-not $env:REMOTEOPS_TARGET_FINGERPRINT){throw 'uia_target_fingerprint_required'}
 $id=Safe-Text $node.Current.AutomationId; $nodeName=Safe-Text $node.Current.Name
 $nodeType=[string]$node.Current.ControlType.ProgrammaticName
 if(((-not $env:REMOTEOPS_AUTOMATION_ID) -or $id -ceq $env:REMOTEOPS_AUTOMATION_ID) -and ((-not $env:REMOTEOPS_NAME) -or $nodeName -ceq $env:REMOTEOPS_NAME) -and (Test-RemoteOpsControlType $nodeType $env:REMOTEOPS_CONTROL_TYPE)){
   if((Get-RemoteOpsControlFingerprint $node $env:REMOTEOPS_WINDOW_FINGERPRINT) -ceq $env:REMOTEOPS_TARGET_FINGERPRINT){return $node}
 }
 $walker=[System.Windows.Automation.TreeWalker]::ControlViewWalker; $child=$walker.GetFirstChild($node)
 while($null -ne $child){$found=Find-RemoteOpsControl $child ($depth+1); if($null -ne $found){return $found}; $child=$walker.GetNextSibling($child)}
 return $null
}
";

/// 在实际执行 UIA 的子进程中重新校验窗口，避免前置观察与输入之间切换了前台。
#[cfg(windows)]
const TARGET_WINDOW_GUARD: &str = r#"
Add-Type @'
using System; using System.Text; using System.Runtime.InteropServices;
public static class RemoteOpsTarget {
 [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
 [DllImport("user32.dll")] public static extern IntPtr GetProcessWindowStation();
 [DllImport("user32.dll")] public static extern IntPtr GetThreadDesktop(uint threadId);
 [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
 [DllImport("user32.dll")] public static extern bool CloseDesktop(IntPtr desktop);
 [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern bool GetUserObjectInformation(IntPtr handle, int index, StringBuilder name, int size, out int needed);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll", SetLastError=true)] public static extern bool GetGUIThreadInfo(uint idThread, ref GUITHREADINFO info);
 [StructLayout(LayoutKind.Sequential)] public struct GUITHREADINFO { public uint cbSize; public uint flags; public IntPtr hwndActive; public IntPtr hwndFocus; public IntPtr hwndCapture; public IntPtr hwndMenuOwner; public IntPtr hwndMoveSize; public IntPtr hwndCaret; public RECT rcCaret; }
 [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint flags);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder text, int count);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT rect);
 [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
'@
function Get-RemoteOpsForeground {
 $handle=[RemoteOpsTarget]::GetForegroundWindow()
 if($handle -ne [IntPtr]::Zero){return [pscustomobject]@{handle=$handle; source='GetForegroundWindow'; gui_error=$null}}
 $info=New-Object RemoteOpsTarget+GUITHREADINFO
 $info.cbSize=[Runtime.InteropServices.Marshal]::SizeOf($info)
 $available=[RemoteOpsTarget]::GetGUIThreadInfo(0,[ref]$info)
 $guiError=if($available){0}else{[Runtime.InteropServices.Marshal]::GetLastWin32Error()}
 if($available -and $info.hwndActive -ne [IntPtr]::Zero){return [pscustomobject]@{handle=$info.hwndActive; source='GetGUIThreadInfo'; gui_error=$guiError}}
 return [pscustomobject]@{handle=[IntPtr]::Zero; source='unavailable'; gui_error=$guiError}
}
function Get-RemoteOpsDesktopContext {
 function Get-ObjectName([IntPtr]$handle) {
  if($handle -eq [IntPtr]::Zero){return ''}
  $buffer=New-Object Text.StringBuilder 512; $needed=0
  if(-not [RemoteOpsTarget]::GetUserObjectInformation($handle,2,$buffer,1024,[ref]$needed)){return ''}
  return $buffer.ToString()
 }
 $session=(Get-Process -Id $PID).SessionId
 $station=Get-ObjectName ([RemoteOpsTarget]::GetProcessWindowStation())
 $desktop=Get-ObjectName ([RemoteOpsTarget]::GetThreadDesktop([RemoteOpsTarget]::GetCurrentThreadId()))
 $input=[RemoteOpsTarget]::OpenInputDesktop(0,$false,1); $inputError=[Runtime.InteropServices.Marshal]::GetLastWin32Error()
 try{$inputName=Get-ObjectName $input}finally{if($input -ne [IntPtr]::Zero){[void][RemoteOpsTarget]::CloseDesktop($input)}}
 $reason=if($session -eq 0){'session_zero'}elseif($station -ine 'WinSta0'){'noninteractive_window_station'}elseif($desktop -ine 'Default' -or $inputName -ine 'Default'){'input_desktop_unavailable_or_secure'}else{$null}
 $foreground=Get-RemoteOpsForeground
 return [pscustomobject]@{interactive=($null -eq $reason); reason=$reason; process_session_id=$session; window_station=$station; thread_desktop=$desktop; input_desktop=$inputName; input_desktop_error=if($input -eq [IntPtr]::Zero){$inputError}else{0}; apartment_state=[string][Threading.Thread]::CurrentThread.ApartmentState; foreground_handle=$foreground.handle.ToInt64(); foreground_source=$foreground.source; gui_thread_info_error=$foreground.gui_error}
}
function Assert-RemoteOpsTarget {
 $context=Get-RemoteOpsDesktopContext
 if(-not $context.interactive){throw $context.reason}
 $handle=(Get-RemoteOpsForeground).handle
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
    stdio_mcp: tokio::sync::Mutex<Option<windows_mcp_stdio::WindowsMcpStdioClient>>,
    stdio_config: Option<windows_provider::WindowsMcpConfig>,
    mcp_configuration_error: Option<String>,
}

#[cfg(windows)]
impl Default for WindowsVisualProvider {
    fn default() -> Self {
        let (mcp_supervisor, mut mcp_configuration_error) =
            match windows_provider::WindowsMcpSupervisor::from_environment() {
                Ok(supervisor) => (supervisor, None),
                Err(error) => {
                    tracing::error!(%error, "Windows-MCP configuration rejected");
                    (None, Some(error))
                }
            };
        let stdio_config = if std::env::var("REMOTEOPS_WINDOWS_MCP_ENABLED")
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            && std::env::var("REMOTEOPS_WINDOWS_MCP_PROTOCOL")
                .map_or(true, |value| !value.eq_ignore_ascii_case("remoteops-pipe"))
        {
            match (
                std::env::var_os("REMOTEOPS_WINDOWS_MCP_PATH"),
                std::env::var("REMOTEOPS_WINDOWS_MCP_SHA256"),
            ) {
                (Some(executable), Ok(sha256)) => {
                    let config = windows_provider::WindowsMcpConfig {
                        executable: executable.into(),
                        sha256,
                    };
                    config.validate().map_or_else(
                        |error| {
                            tracing::error!(%error, "Windows-MCP stdio configuration rejected");
                            None
                        },
                        |()| Some(config),
                    )
                }
                _ => None,
            }
        } else {
            None
        };
        let stdio_enabled = std::env::var("REMOTEOPS_WINDOWS_MCP_ENABLED")
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            && std::env::var("REMOTEOPS_WINDOWS_MCP_PROTOCOL")
                .map_or(true, |value| !value.eq_ignore_ascii_case("remoteops-pipe"));
        if stdio_enabled && stdio_config.is_none() && mcp_configuration_error.is_none() {
            mcp_configuration_error =
                Some("已启用 Windows-MCP stdio，但路径或 SHA-256 配置无效".to_owned());
        }
        Self {
            mcp_supervisor: tokio::sync::Mutex::new(mcp_supervisor),
            stdio_mcp: tokio::sync::Mutex::new(None),
            stdio_config,
            mcp_configuration_error,
        }
    }
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
        drop(supervisor);
        if let Some(config) = self.stdio_config.as_ref() {
            let mut client = self.stdio_mcp.lock().await;
            if client.as_mut().is_some_and(|client| !client.is_usable())
                && let Some(mut stopped) = client.take()
            {
                stopped.stop().await;
            }
            if client.is_none() {
                let mut started = windows_mcp_stdio::WindowsMcpStdioClient::start(config)
                    .await
                    .map_err(|error| {
                        VisualProviderError::Protocol(format!(
                            "Windows-MCP stdio 启动失败：{error}"
                        ))
                    })?;
                let tools = started.list_tools().await.map_err(|error| {
                    VisualProviderError::Protocol(format!("Windows-MCP 工具发现失败：{error}"))
                })?;
                if !tools.iter().any(|tool| tool == "Snapshot")
                    || !tools.iter().any(|tool| tool == "Screenshot")
                {
                    started.stop().await;
                    return Err(VisualProviderError::Protocol(
                        "Windows-MCP 缺少 Snapshot/Screenshot 工具".into(),
                    ));
                }
                *client = Some(started);
            }
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
        if self.stdio_config.is_some() {
            return Ok(None);
        }
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
            target_fingerprint,
            ..
        } = target
        else {
            return Err(VisualProviderError::Rejected(
                "坐标目标必须经过单独审批，当前拒绝回退输入".into(),
            ));
        };
        if !matches!(action, "invoke" | "click" | "submit" | "press_enter") {
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
if(-not $env:REMOTEOPS_AUTOMATION_ID -and -not $env:REMOTEOPS_NAME){throw 'UIA 目标缺少 automation_id 或 name'}
$element=Find-RemoteOpsControl $root 0
if($null -eq $element){throw '前台窗口中未找到 UIA 目标'}
$action=$env:REMOTEOPS_UIA_ACTION
if($action -eq 'press_enter') {
  Add-Type -AssemblyName System.Windows.Forms
  [void]$element.SetFocus(); [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
} else {
  $pattern=$element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern); [void](Assert-RemoteOpsTarget); $pattern.Invoke()
}
if($comResult -ge 0){[RemoteOpsCom]::CoUninitialize()}; [pscustomobject]@{ok=$true}|ConvertTo-Json -Compress
"#;
        let script = format!(
            "{TARGET_WINDOW_GUARD}
{UIA_CONTROL_HELPERS}
{script}"
        );
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
        command.env("REMOTEOPS_TARGET_FINGERPRINT", target_fingerprint);
        command.env("REMOTEOPS_UIA_ACTION", action);
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
            target_fingerprint,
            control_type,
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
$element=Find-RemoteOpsControl $root 0
if($null -eq $element){throw '前台窗口中未找到 UIA 文本目标'}
$pattern=$element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern); if($pattern.Current.IsReadOnly){throw 'UIA 文本控件为只读'}; [void](Assert-RemoteOpsTarget); $pattern.SetValue($env:REMOTEOPS_TEXT); if($pattern.Current.Value -cne $env:REMOTEOPS_TEXT){throw 'UIA value verification failed'}; if($comResult -ge 0){[RemoteOpsCom]::CoUninitialize()}; [pscustomobject]@{ok=$true}|ConvertTo-Json -Compress
"#;
        let script = format!(
            "{TARGET_WINDOW_GUARD}
{UIA_CONTROL_HELPERS}
{script}"
        );
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
        command.env("REMOTEOPS_TARGET_FINGERPRINT", target_fingerprint);
        command.env("REMOTEOPS_TEXT", text);
        let output = run_desktop_command(command).await?;
        if !output.status.success() {
            return Err(VisualProviderError::Rejected(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn send_coordinate_input(
        &self,
        target: &VisualTarget,
        input: &str,
        observation: &VisualObservation,
    ) -> Result<(), VisualProviderError> {
        let VisualTarget::Coordinate {
            x,
            y,
            window_fingerprint,
            screenshot_scale_percent,
            end_x,
            end_y,
            display_id,
            ..
        } = target
        else {
            return Err(VisualProviderError::Rejected(
                "非坐标目标禁止使用鼠标键盘回退".into(),
            ));
        };
        if *screenshot_scale_percent == 0 {
            return Err(VisualProviderError::Rejected("截图缩放比例无效".into()));
        }
        let is_drag = matches!(input, "drag" | "drag_left");
        let endpoint = match (end_x, end_y) {
            (Some(end_x), Some(end_y)) => Some((*end_x, *end_y)),
            (None, None) => None,
            _ => {
                return Err(VisualProviderError::Rejected(
                    "拖拽终点必须同时提供 end_x 和 end_y".into(),
                ));
            }
        };
        if is_drag != endpoint.is_some() {
            return Err(VisualProviderError::Rejected(
                if is_drag {
                    "拖拽输入缺少终点"
                } else {
                    "只有拖拽输入允许提供终点"
                }
                .into(),
            ));
        }
        let supported = is_supported_input(input);
        if !supported {
            return Err(VisualProviderError::Rejected("不支持的图形输入".into()));
        }
        let display = observation
            .displays
            .iter()
            .find(|display| &display.display_id == display_id)
            .ok_or_else(|| VisualProviderError::Rejected("目标显示器不存在".into()))?;
        let start = map_screenshot_point(*x, *y, display, *screenshot_scale_percent)?;
        let end = endpoint
            .map(|(x, y)| map_screenshot_point(x, y, display, *screenshot_scale_percent))
            .transpose()?;
        let script = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
if (-not ('RemoteOpsInput' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public static class RemoteOpsInput {
 [DllImport("user32.dll", SetLastError=true)] public static extern bool SetCursorPos(int x, int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, int data, UIntPtr extra);
}
'@ }
[void](Assert-RemoteOpsTarget)
if(-not [RemoteOpsInput]::SetCursorPos([int]$env:REMOTEOPS_X,[int]$env:REMOTEOPS_Y)){throw '无法定位鼠标'}
[void](Assert-RemoteOpsTarget)
$inputName=$env:REMOTEOPS_INPUT
switch -Regex ($inputName) {
 '^move$' { break }
 '^(click|left_click)$' { [RemoteOpsInput]::mouse_event(0x0002,0,0,0,[UIntPtr]::Zero); [RemoteOpsInput]::mouse_event(0x0004,0,0,0,[UIntPtr]::Zero); break }
 '^double_click$' { 1..2 | ForEach-Object { [RemoteOpsInput]::mouse_event(0x0002,0,0,0,[UIntPtr]::Zero); [RemoteOpsInput]::mouse_event(0x0004,0,0,0,[UIntPtr]::Zero); if($_ -eq 1){Start-Sleep -Milliseconds 80} }; break }
 '^right_click$' { [RemoteOpsInput]::mouse_event(0x0008,0,0,0,[UIntPtr]::Zero); [RemoteOpsInput]::mouse_event(0x0010,0,0,0,[UIntPtr]::Zero); break }
 '^middle_click$' { [RemoteOpsInput]::mouse_event(0x0020,0,0,0,[UIntPtr]::Zero); [RemoteOpsInput]::mouse_event(0x0040,0,0,0,[UIntPtr]::Zero); break }
 '^wheel_up$' { [RemoteOpsInput]::mouse_event(0x0800,0,0,120,[UIntPtr]::Zero); break }
 '^wheel_down$' { [RemoteOpsInput]::mouse_event(0x0800,0,0,-120,[UIntPtr]::Zero); break }
 '^drag(_left)?$' {
   if($null -eq $env:REMOTEOPS_END_X -or $null -eq $env:REMOTEOPS_END_Y){throw '拖拽终点缺失'}
   [RemoteOpsInput]::mouse_event(0x0002,0,0,0,[UIntPtr]::Zero)
   $sx=[int]$env:REMOTEOPS_X; $sy=[int]$env:REMOTEOPS_Y; $ex=[int]$env:REMOTEOPS_END_X; $ey=[int]$env:REMOTEOPS_END_Y
   1..8 | ForEach-Object { $t=$_ / 8.0; $nx=[int][math]::Round($sx + (($ex-$sx)*$t)); $ny=[int][math]::Round($sy + (($ey-$sy)*$t)); if(-not [RemoteOpsInput]::SetCursorPos($nx,$ny)){throw '无法移动拖拽指针'}; Start-Sleep -Milliseconds 15 }
   [RemoteOpsInput]::mouse_event(0x0004,0,0,0,[UIntPtr]::Zero); break
 }
 '^key:(.+)$' {
   Add-Type -AssemblyName System.Windows.Forms
   $key=$inputName.Substring(4); $upper=$key.ToUpperInvariant()
   $sendKey=switch ($upper) {
     'ENTER' {'{ENTER}'} 'TAB' {'{TAB}'} 'SHIFT+TAB' {'+{TAB}'} 'ESC' {'{ESC}'} 'ESCAPE' {'{ESC}'} 'BACKSPACE' {'{BACKSPACE}'} 'DELETE' {'{DELETE}'} 'UP' {'{UP}'} 'DOWN' {'{DOWN}'} 'LEFT' {'{LEFT}'} 'RIGHT' {'{RIGHT}'} 'HOME' {'{HOME}'} 'END' {'{END}'} 'SPACE' {' '}
     'CTRL+L' {'^l'} 'CTRL+C' {'^c'} 'CTRL+V' {'^v'} 'CTRL+A' {'^a'} 'CTRL+Z' {'^z'} 'CTRL+Y' {'^y'} 'CTRL+W' {'^w'} 'CTRL+TAB' {'^{TAB}'} 'ALT+F4' {'%{F4}'} 'ALT+TAB' {'%{TAB}'}
     default { if($key.Length -eq 1 -and $key -match '^[A-Za-z0-9]$'){ $key } elseif($upper -match '^F([1-9]|1[0-2])$'){ '{' + $upper + '}' } else { throw '不支持的键盘输入' } }
   }
   [System.Windows.Forms.SendKeys]::SendWait($sendKey); break
 }
 default { throw '不支持的图形输入' }
}
[void](Assert-RemoteOpsTarget)
[pscustomobject]@{ok=$true}|ConvertTo-Json -Compress
"#;
        let script = format!(
            "{TARGET_WINDOW_GUARD}
{script}"
        );
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
        command.env("REMOTEOPS_X", start.0.to_string());
        command.env("REMOTEOPS_WINDOW_FINGERPRINT", window_fingerprint);
        command.env("REMOTEOPS_Y", start.1.to_string());
        if let Some((end_x, end_y)) = end {
            command.env("REMOTEOPS_END_X", end_x.to_string());
            command.env("REMOTEOPS_END_Y", end_y.to_string());
        }
        command.env("REMOTEOPS_INPUT", input);
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
        // 上游文本仅用于状态证据；安全窗口指纹仍由下方原生句柄采集产生。
        let upstream_snapshot = if self.stdio_config.is_some() {
            let mut client = self.stdio_mcp.lock().await;
            Some(
                client
                    .as_mut()
                    .ok_or_else(|| {
                        VisualProviderError::Protocol("Windows-MCP stdio 尚未初始化".into())
                    })?
                    .snapshot(include_ui_tree)
                    .await
                    .map_err(|error| {
                        VisualProviderError::Protocol(format!("Windows-MCP 观察失败：{error}"))
                    })?,
            )
        } else {
            None
        };
        if let Some(observation) = self
            .observe_external_mcp(request_id, session_id, include_screenshot, include_ui_tree)
            .await?
        {
            return Ok(observation);
        }
        let script = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$desktopContext=Get-RemoteOpsDesktopContext
if(-not $desktopContext.interactive){
 [pscustomobject]@{state='no_interactive_desktop'; displays=@(); windows=@(); ui_tree=[pscustomobject]@{available=$false; reason=$desktopContext.reason; diagnostics=$desktopContext; children=@()}} | ConvertTo-Json -Compress -Depth 8
 exit 0
}
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
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT point);
}
'@ }
if ($env:REMOTEOPS_INCLUDE_SCREENSHOT -eq '1') { Add-Type -AssemblyName System.Drawing }
$screens = [System.Windows.Forms.Screen]::AllScreens
$displays = @($screens | ForEach-Object {
  [pscustomobject]@{ display_id=$_.DeviceName; physical_width=$_.Bounds.Width; physical_height=$_.Bounds.Height; logical_width=$_.Bounds.Width; logical_height=$_.Bounds.Height; dpi=96; scale_percent=100; origin_x=$_.Bounds.X; origin_y=$_.Bounds.Y; physical_origin_x=$_.Bounds.X; physical_origin_y=$_.Bounds.Y }
})
$windows = [System.Collections.Generic.List[object]]::new(); $foreground = (Get-RemoteOpsForeground).handle; $activeFingerprint = $null
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
  function Convert-Uia([System.Windows.Automation.AutomationElement]$e,[int]$depth,[string]$windowFingerprint) {
    if($null -eq $e){return $null}
    $nodeName=Safe-Text $e.Current.Name; $nodeId=Safe-Text $e.Current.AutomationId; $nodeType=Safe-Text $e.Current.ControlType.ProgrammaticName
    $nodeFingerprint=Get-RemoteOpsControlFingerprint $e $windowFingerprint
    $n=[pscustomobject]@{name=$nodeName; automation_id=$nodeId; control_type=$nodeType; target_fingerprint=$nodeFingerprint; children=@()}
    # Explorer 的磁盘/文件项目通常位于 ListView -> ListItem 的第四层；
    # 保留硬上限，避免大型目录导致观察结果失控。
    if($depth -ge 6){return $n}
    $walker=if($nodeId -eq 'listview'){
      [System.Windows.Automation.TreeWalker]::RawViewWalker
    } else {
      [System.Windows.Automation.TreeWalker]::ControlViewWalker
    }
    $c=$walker.GetFirstChild($e); $list=@(); $visited=0
    while($null -ne $c -and $visited -lt 120){
      $visited++; $child=Convert-Uia $c ($depth+1) $windowFingerprint
      if($null -ne $child){$list += $child}
      $c=$walker.GetNextSibling($c)
    }
    $n.children=$list
    return $n
  }
  $comResult=[RemoteOpsCom]::CoInitializeEx([IntPtr]::Zero,0x2); if ($comResult -lt 0) { throw 'UIA COM initialization failed' }
  try {
    $root=[System.Windows.Automation.AutomationElement]::FromHandle($foreground)
    if ($null -eq $root) { $ui=[pscustomobject]@{available=$false; provider='windows-powershell'; reason='uia_root_unavailable'; children=@()} }
    else { $ui=Convert-Uia $root 0 $script:activeFingerprint; if ($null -ne $ui) { $ui | Add-Member -NotePropertyName available -NotePropertyValue $true }; if ($null -eq $ui) { $ui=[pscustomobject]@{available=$false; provider='windows-powershell'; reason='uia_tree_unavailable'; children=@()} } }
  } catch { $ui=[pscustomobject]@{available=$false; provider='windows-powershell'; reason=(Safe-Text $_.Exception.Message); children=@()} }
  finally { if ($comResult -ge 0) { [RemoteOpsCom]::CoUninitialize() } }
  }
}
$shot = $null; $w = $null; $h = $null; $screenshotError = $null
if ($env:REMOTEOPS_INCLUDE_SCREENSHOT -eq '1' -and $screens.Count -gt 0) {
  try {
    $b = $screens[0].Bounds; $w=$b.Width; $h=$b.Height
    $bmp = New-Object System.Drawing.Bitmap($w,$h); $g=[System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size); $ms=New-Object System.IO.MemoryStream
    $bmp.Save($ms,[System.Drawing.Imaging.ImageFormat]::Png); $shot=[Convert]::ToBase64String($ms.ToArray())
  } catch { $screenshotError = (Safe-Text $_.Exception.Message); $shot = $null; $w = $null; $h = $null }
  finally { if ($null -ne $g) { $g.Dispose() }; if ($null -ne $bmp) { $bmp.Dispose() }; if ($null -ne $ms) { $ms.Dispose() } }
}
if($null -ne $ui){$ui | Add-Member -NotePropertyName diagnostics -NotePropertyValue $desktopContext}
if($null -ne $ui -and $null -ne $screenshotError){$ui | Add-Member -NotePropertyName screenshot_error -NotePropertyValue $screenshotError}
[void]0
$cursor = $null
try { $p = New-Object RemoteOpsUser32+POINT; if ([RemoteOpsUser32]::GetCursorPos([ref]$p)) { $cursor = [pscustomobject]@{ x=$p.X; y=$p.Y } } } catch { $cursor = $null }
[pscustomobject]@{ state=$state; state_reason=$stateReason; displays=$displays; windows=$windows; active_window_fingerprint=$script:activeFingerprint; screenshot_base64=$shot; screenshot_width=$w; screenshot_height=$h; cursor=$cursor; ui_tree=$ui } | ConvertTo-Json -Compress -Depth 8
"#;
        let script = format!(
            "{TARGET_WINDOW_GUARD}
{UIA_CONTROL_HELPERS}
{script}"
        );
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
        let value: serde_json::Value = serde_json::from_str(&output_text)
            .map_err(|e| VisualProviderError::Protocol(format!("桌面采集结果无效：{e}")))?;
        let displays = serde_json::from_value(value.get("displays").cloned().unwrap_or_default())
            .map_err(|error| {
            VisualProviderError::Protocol(format!("显示器采集结构无效：{error}"))
        })?;
        let windows = serde_json::from_value(value.get("windows").cloned().unwrap_or_default())
            .map_err(|error| VisualProviderError::Protocol(format!("窗口采集结构无效：{error}")))?;
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
        let cursor_x = value.get("cursor").and_then(|v| v.get("x")).and_then(serde_json::Value::as_i64).and_then(|v| i32::try_from(v).ok());
        let cursor_y = value.get("cursor").and_then(|v| v.get("y")).and_then(serde_json::Value::as_i64).and_then(|v| i32::try_from(v).ok());
        let state = value
            .get("state")
            .cloned()
            .and_then(|state| serde_json::from_value(state).ok())
            .unwrap_or(remoteops_domain::VisualSessionState::NoInteractiveDesktop);
        let mut ui_tree = value.get("ui_tree").cloned().filter(|v| !v.is_null());
        if let Some(tree) = ui_tree.as_mut().and_then(serde_json::Value::as_object_mut) {
            tree.insert("provider".into(), serde_json::json!("windows-native-uia"));
            if let Some(snapshot) = upstream_snapshot {
                tree.insert(
                    "upstream".into(),
                    serde_json::json!({
                        "provider":"windows-mcp-stdio", "initialized":true, "snapshot":snapshot
                    }),
                );
            }
        }
        Ok(VisualObservation {
            request_id,
            session_id,
            provider_instance_id: "windows-powershell-desktop".into(),
            state,
            windows,
            displays,
            active_window_fingerprint,
            ui_tree,
            screenshot_base64,
            screenshot_width,
            screenshot_height,
            cursor_x,
            cursor_y,
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
        condition: &str,
        timeout_millis: u64,
    ) -> Result<VisualObservation, VisualProviderError> {
        let condition = remoteops_domain::VisualWaitCondition::parse(condition)
            .map_err(VisualProviderError::Rejected)?;
        let timeout_millis = remoteops_domain::normalize_visual_wait_timeout(Some(timeout_millis))
            .map_err(VisualProviderError::Rejected)?;
        let include_screenshot = matches!(
            condition,
            remoteops_domain::VisualWaitCondition::Hash { .. }
        );
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_millis);
        loop {
            let observation = self
                .observe_desktop(request_id, session_id, include_screenshot, true)
                .await?;
            if condition.matches(&observation) {
                return Ok(observation);
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(VisualProviderError::Rejected(
                    "图形等待超时：条件未满足".to_owned(),
                ));
            }
            tokio::time::sleep_until(std::cmp::min(
                deadline,
                now + std::time::Duration::from_millis(250),
            ))
            .await;
        }
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
        let (action, embedded_endpoint) = parse_visual_input(input)?;
        let effective_target = target_with_drag_endpoint(target, embedded_endpoint)?;
        let before = self
            .verify_foreground_target(request_id, session_id, &effective_target)
            .await?;
        let VisualTarget::Coordinate { display_id, .. } = &effective_target else {
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
        if !effective_target.is_valid_for_display(display) {
            return Err(VisualProviderError::Rejected(
                "坐标目标或拖拽终点超出目标显示器边界".into(),
            ));
        }
        self.send_coordinate_input(&effective_target, action, &before)
            .await?;
        let observation = self
            .observe_desktop(request_id, session_id, false, true)
            .await?;
        let effect_verified = if matches!(action, "move" | "drag" | "drag_left") {
            cursor_effect_verified(&effective_target, action, display, &observation)
        } else {
            observed_uia_change(&before, &observation)
        };
        Ok(VisualActionResult {
            request_id,
            session_id,
            action_sent: true,
            effect_verified,
            observation: Some(observation),
            error_code: (!effect_verified).then(|| "effect_not_observable".to_owned()),
            message: if effect_verified {
                "坐标回退输入已发送并完成 UIA 后置观察".into()
            } else {
                "坐标回退输入已发送；当前 Provider 未取得可证明的界面变化".into()
            },
        })
    }
    async fn stop(&self, _session_id: SessionId) -> Result<(), VisualProviderError> {
        if let Some(mut client) = self.stdio_mcp.lock().await.take() {
            client.stop().await;
        }
        if let Some(supervisor) = self.mcp_supervisor.lock().await.as_mut() {
            supervisor.stop().await;
        }
        Ok(())
    }
}

type VisualInputParts<'a> = (&'a str, Option<(i32, i32)>);

fn parse_visual_input(input: &str) -> Result<VisualInputParts<'_>, VisualProviderError> {
    if let Some(value) = input.strip_prefix("drag_to:") {
        let (x, y) = value
            .split_once(',')
            .ok_or_else(|| VisualProviderError::Rejected("拖拽终点格式无效".into()))?;
        let x = x
            .parse::<i32>()
            .map_err(|_| VisualProviderError::Rejected("拖拽终点 X 无效".into()))?;
        let y = y
            .parse::<i32>()
            .map_err(|_| VisualProviderError::Rejected("拖拽终点 Y 无效".into()))?;
        return Ok(("drag", Some((x, y))));
    }
    Ok((input, None))
}

fn target_with_drag_endpoint(
    target: &VisualTarget,
    endpoint: Option<(i32, i32)>,
) -> Result<VisualTarget, VisualProviderError> {
    let Some((end_x, end_y)) = endpoint else {
        return Ok(target.clone());
    };
    let VisualTarget::Coordinate {
        window_fingerprint,
        display_id,
        x,
        y,
        screenshot_scale_percent,
        end_x: current_end_x,
        end_y: current_end_y,
    } = target
    else {
        return Err(VisualProviderError::Rejected(
            "拖拽终点只能用于坐标目标".into(),
        ));
    };
    if current_end_x.is_some() || current_end_y.is_some() {
        return Err(VisualProviderError::Rejected(
            "拖拽终点重复提供".into(),
        ));
    }
    Ok(VisualTarget::Coordinate {
        window_fingerprint: window_fingerprint.clone(),
        display_id: display_id.clone(),
        x: *x,
        y: *y,
        screenshot_scale_percent: *screenshot_scale_percent,
        end_x: Some(end_x),
        end_y: Some(end_y),
    })
}

fn is_supported_input(input: &str) -> bool {
    matches!(
        input,
        "move"
            | "click"
            | "left_click"
            | "double_click"
            | "right_click"
            | "middle_click"
            | "wheel_up"
            | "wheel_down"
            | "drag"
            | "drag_left"
    ) || input
        .strip_prefix("key:")
        .is_some_and(|key| !key.is_empty())
}

#[cfg(windows)]
fn map_screenshot_point(
    x: i32,
    y: i32,
    display: &remoteops_domain::VisualDisplay,
    screenshot_scale_percent: u32,
) -> Result<(i32, i32), VisualProviderError> {
    if screenshot_scale_percent == 0 || display.logical_width == 0 || display.logical_height == 0 {
        return Err(VisualProviderError::Rejected("显示器缩放信息无效".into()));
    }
    let logical_x = i64::from(display.origin_x)
        + ((i64::from(x) - i64::from(display.origin_x)) * 100
            + i64::from(screenshot_scale_percent) / 2)
            / i64::from(screenshot_scale_percent);
    let logical_y = i64::from(display.origin_y)
        + ((i64::from(y) - i64::from(display.origin_y)) * 100
            + i64::from(screenshot_scale_percent) / 2)
            / i64::from(screenshot_scale_percent);
    let logical_x = i32::try_from(logical_x)
        .map_err(|_| VisualProviderError::Rejected("X 坐标超出整数范围".into()))?;
    let logical_y = i32::try_from(logical_y)
        .map_err(|_| VisualProviderError::Rejected("Y 坐标超出整数范围".into()))?;
    if !display.contains_logical_point(logical_x, logical_y) {
        return Err(VisualProviderError::Rejected(
            "坐标超出目标显示器边界".into(),
        ));
    }
    let physical_x = i64::from(display.physical_origin_x)
        + (i64::from(logical_x - display.origin_x) * i64::from(display.physical_width)
            + i64::from(display.logical_width) / 2)
            / i64::from(display.logical_width);
    let physical_y = i64::from(display.physical_origin_y)
        + (i64::from(logical_y - display.origin_y) * i64::from(display.physical_height)
            + i64::from(display.logical_height) / 2)
            / i64::from(display.logical_height);
    Ok((
        i32::try_from(physical_x)
            .map_err(|_| VisualProviderError::Rejected("物理 X 坐标超出整数范围".into()))?,
        i32::try_from(physical_y)
            .map_err(|_| VisualProviderError::Rejected("物理 Y 坐标超出整数范围".into()))?,
    ))
}

#[cfg(windows)]
fn cursor_effect_verified(
    target: &VisualTarget,
    input: &str,
    display: &remoteops_domain::VisualDisplay,
    observation: &VisualObservation,
) -> bool {
    let Some(((start_x, start_y), end)) = target.coordinate_points() else {
        return false;
    };
    let point = if matches!(input, "drag" | "drag_left") {
        end.unwrap_or((start_x, start_y))
    } else {
        (start_x, start_y)
    };
    let Ok((expected_x, expected_y)) = map_screenshot_point(
        point.0,
        point.1,
        display,
        match target {
            VisualTarget::Coordinate { screenshot_scale_percent, .. } => *screenshot_scale_percent,
            VisualTarget::Control { .. } => return false,
        },
    ) else {
        return false;
    };
    let (Some(actual_x), Some(actual_y)) = (observation.cursor_x, observation.cursor_y) else {
        return false;
    };
    (actual_x - expected_x).abs() <= 2 && (actual_y - expected_y).abs() <= 2
}

/// 仅将同一前台窗口中可见的 UIA 变化作为动作效果证据。
#[cfg(windows)]
fn observed_uia_change(before: &VisualObservation, after: &VisualObservation) -> bool {
    after.state == remoteops_domain::VisualSessionState::Ready
        && before.active_window_fingerprint.is_some()
        && same_observed_window(before, after)
        && before
            .ui_tree
            .as_ref()
            .is_some_and(|tree| tree.get("available") == Some(&serde_json::Value::Bool(true)))
        && after
            .ui_tree
            .as_ref()
            .is_some_and(|tree| tree.get("available") == Some(&serde_json::Value::Bool(true)))
        && before.ui_tree.as_ref().map(native_uia_state)
            != after.ui_tree.as_ref().map(native_uia_state)
}

/// 标题或位置变化会刷新窗口指纹，使用两次观察中的原生句柄和进程核对同一窗口。
#[cfg(windows)]
fn same_observed_window(before: &VisualObservation, after: &VisualObservation) -> bool {
    if before.active_window_fingerprint == after.active_window_fingerprint {
        return before.active_window_fingerprint.is_some();
    }
    let find = |observation: &VisualObservation| {
        observation
            .windows
            .iter()
            .find(|window| {
                Some(window.fingerprint.as_str())
                    == observation.active_window_fingerprint.as_deref()
            })
            .map(|window| {
                (
                    window.window_id.clone(),
                    window.process_id,
                    window.session_id.clone(),
                )
            })
    };
    let previous = find(before);
    previous.is_some() && previous == find(after)
}

/// 剔除上游光标、其他窗口和诊断元信息；它们不能证明目标控件操作生效。
#[cfg(windows)]
fn native_uia_state(node: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "name":node.get("name"),
        "automation_id":node.get("automation_id"),
        "control_type":node.get("control_type"),
        "children":node.get("children").and_then(serde_json::Value::as_array)
            .map(|children| children.iter().map(native_uia_state).collect::<Vec<_>>())
    })
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
            cursor_x: None,
            cursor_y: None,
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

    #[test]
    fn keyboard_input_requires_non_empty_key_name() {
        assert!(is_supported_input("key:ENTER"));
        assert!(is_supported_input("key:CTRL+A"));
        assert!(!is_supported_input("key:"));
        assert!(!is_supported_input("keyboard:ENTER"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn runtime_ids_distinguish_duplicate_controls_and_preserve_unicode() {
        let script = format!(
            r"{UIA_CONTROL_HELPERS}
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$a=[pscustomobject]@{{Current=[pscustomobject]@{{Name=('Control'+[char]0xD83D+[char]0xDE80);AutomationId='same';ControlType=[pscustomobject]@{{ProgrammaticName='ControlType.Button'}}}}}}
$b=[pscustomobject]@{{Current=$a.Current}}
$a | Add-Member -MemberType ScriptMethod -Name GetRuntimeId -Value {{return @(1,2)}}
$b | Add-Member -MemberType ScriptMethod -Name GetRuntimeId -Value {{return @(1,3)}}
[pscustomobject]@{{first=(Get-RemoteOpsControlFingerprint $a 'window');repeat=(Get-RemoteOpsControlFingerprint $a 'window');second=(Get-RemoteOpsControlFingerprint $b 'window');name=(Safe-Text $a.Current.Name);short_type=(Test-RemoteOpsControlType 'ControlType.Button' 'button')}} | ConvertTo-Json -Compress
"
        );
        let mut command = hidden_powershell_command();
        command.args(["-NoProfile", "-NonInteractive", "-STA", "-Command", &script]);
        let output = run_desktop_command(command).await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["first"], value["repeat"]);
        assert_ne!(value["first"], value["second"]);
        assert_eq!(value["name"], "Control🚀");
        assert_eq!(value["short_type"], true);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn foreground_probe_has_complete_native_layout_and_honest_source() {
        let script = format!(
            r"$ErrorActionPreference='Stop'
{TARGET_WINDOW_GUARD}
$info=New-Object RemoteOpsTarget+GUITHREADINFO
$size=[Runtime.InteropServices.Marshal]::SizeOf($info)
$offset=[Runtime.InteropServices.Marshal]::OffsetOf([RemoteOpsTarget+GUITHREADINFO],'rcCaret').ToInt64()
$info.cbSize=$size
$success=[RemoteOpsTarget]::GetGUIThreadInfo(0,[ref]$info)
$errorCode=if($success){{0}}else{{[Runtime.InteropServices.Marshal]::GetLastWin32Error()}}
$foreground=Get-RemoteOpsForeground
[pscustomobject]@{{size=$size; caret_offset=$offset; pointer_size=[IntPtr]::Size; error_code=$errorCode; handle=$foreground.handle.ToInt64(); source=$foreground.source}} | ConvertTo-Json -Compress
"
        );
        let mut command = hidden_powershell_command();
        command.args(["-NoProfile", "-NonInteractive", "-STA", "-Command", &script]);
        let output = run_desktop_command(command).await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let pointer_size = value["pointer_size"].as_u64().unwrap();
        assert_eq!(value["size"], 24 + 6 * pointer_size);
        assert_eq!(value["caret_offset"], 8 + 6 * pointer_size);
        // ERROR_INVALID_PARAMETER 会暴露遗漏 rcCaret 导致的错误 cbSize。
        assert_ne!(value["error_code"], 87);
        if value["handle"] == 0 {
            assert_eq!(value["source"], "unavailable");
        } else {
            assert!(matches!(
                value["source"].as_str(),
                Some("GetForegroundWindow" | "GetGUIThreadInfo")
            ));
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "需要已登录的 Windows 交互桌面；仅做只读观察"]
    async fn native_interactive_observation_reports_windows_and_uia_diagnostics() {
        let provider = WindowsVisualProvider {
            mcp_supervisor: tokio::sync::Mutex::new(None),
            stdio_mcp: tokio::sync::Mutex::new(None),
            stdio_config: None,
            mcp_configuration_error: None,
        };
        let observation = provider
            .observe(RequestId::new(), SessionId::new(), false, true)
            .await
            .unwrap();
        assert_eq!(
            observation.state,
            remoteops_domain::VisualSessionState::Ready
        );
        assert!(!observation.windows.is_empty());
        assert!(observation.active_window_fingerprint.is_some());
        let tree = observation.ui_tree.unwrap();
        assert!(tree["diagnostics"]["process_session_id"].as_u64().unwrap() > 0);
        assert_eq!(tree["diagnostics"]["window_station"], "WinSta0");
        assert_eq!(tree["diagnostics"]["apartment_state"], "STA");
        assert_eq!(
            tree["available"], true,
            "UIA 不可用原因：{}",
            tree["reason"]
        );
        assert_eq!(tree["target_fingerprint"].as_str().unwrap().len(), 64);
    }

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
        let mut metadata_only = before.clone();
        metadata_only.ui_tree.as_mut().unwrap()["upstream"] =
            serde_json::json!({"snapshot":"Cursor Position: (100, 200)"});
        assert!(!observed_uia_change(&before, &metadata_only));
        metadata_only.ui_tree.as_mut().unwrap()["diagnostics"] =
            serde_json::json!({"capture_ms":50});
        assert!(!observed_uia_change(&before, &metadata_only));
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
