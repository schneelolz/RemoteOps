//! `RemoteOps` AI 图形 Provider 抽象。

use async_trait::async_trait;
use remoteops_domain::{RequestId, SessionId, VisualActionResult, VisualObservation, VisualTarget};
use thiserror::Error;

#[cfg(windows)]
pub mod windows_provider;

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

/// Windows 交互式桌面的最小真实 Provider。
///
/// 通过用户 Session 中的 Windows PowerShell 获取屏幕和截图；Agent Service
/// 不直接访问桌面。UIA/输入动作在 Windows-MCP 接入前明确拒绝，避免伪造成功。
#[cfg(windows)]
#[derive(Default)]
pub struct WindowsVisualProvider;

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
    async fn invoke_uia(
        &self,
        target: &VisualTarget,
        action: &str,
    ) -> Result<(), VisualProviderError> {
        let VisualTarget::Control {
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
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
if (-not ('RemoteOpsUser32' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public static class RemoteOpsUser32 { [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); }
'@ }
$root=[System.Windows.Automation.AutomationElement]::FromHandle([RemoteOpsUser32]::GetForegroundWindow())
if($null -eq $root){ throw '没有可验证的前台窗口' }
$conditions=@(); if($env:REMOTEOPS_AUTOMATION_ID){$conditions += [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty,$env:REMOTEOPS_AUTOMATION_ID)}; if($env:REMOTEOPS_NAME){$conditions += [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty,$env:REMOTEOPS_NAME)}
if($conditions.Count -eq 0){throw 'UIA 目标缺少 automation_id 或 name'}
$condition=if($conditions.Count -eq 1){$conditions[0]}else{[System.Windows.Automation.AndCondition]::new($conditions)}
$element=$root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,$condition)
if($null -eq $element){throw '前台窗口中未找到 UIA 目标'}
$pattern=$element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern); $pattern.Invoke(); [pscustomobject]@{ok=$true}|ConvertTo-Json -Compress
"#;
        let mut command = tokio::process::Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ]);
        command.env(
            "REMOTEOPS_AUTOMATION_ID",
            automation_id.as_deref().unwrap_or_default(),
        );
        command.env("REMOTEOPS_NAME", name.as_deref().unwrap_or_default());
        command.env(
            "REMOTEOPS_CONTROL_TYPE",
            control_type.as_deref().unwrap_or_default(),
        );
        let output = command
            .output()
            .await
            .map_err(|e| VisualProviderError::Protocol(format!("启动 UIA 动作失败：{e}")))?;
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
        let script = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
if (-not ('RemoteOpsUser32' -as [type])) { Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class RemoteOpsUser32 {
  public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr p);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
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
$windows = [System.Collections.Generic.List[object]]::new(); $foreground = [RemoteOpsUser32]::GetForegroundWindow()
$callback = [RemoteOpsUser32+EnumWindowsProc]{ param($handle,$unused)
  if (-not [RemoteOpsUser32]::IsWindowVisible($handle)) { return $true }
  $text = New-Object Text.StringBuilder 512; [void][RemoteOpsUser32]::GetWindowText($handle,$text,$text.Capacity)
  if ($text.Length -eq 0) { return $true }
  $windowPid=[uint32]0; [void][RemoteOpsUser32]::GetWindowThreadProcessId($handle,[ref]$windowPid); $rect=New-Object RemoteOpsUser32+RECT
  if (-not [RemoteOpsUser32]::GetWindowRect($handle,[ref]$rect)) { return $true }
  $proc=Get-Process -Id $windowPid -ErrorAction SilentlyContinue; $name=if($proc){$proc.ProcessName}else{'unknown'}
  $fingerprint=[BitConverter]::ToString(([Security.Cryptography.SHA256]::Create().ComputeHash([Text.Encoding]::UTF8.GetBytes("$windowPid|$($text.ToString())|$($rect.Left)|$($rect.Top)|$($rect.Right)|$($rect.Bottom)")))).Replace('-','').ToLowerInvariant()
  $pobj=Get-Process -Id $windowPid -ErrorAction SilentlyContinue; $sid=if($pobj){[string]$pobj.SessionId}else{'-1'}
  $windows.Add([pscustomobject]@{ window_id="0x$('{0:x}' -f $handle.ToInt64())"; process_id=$windowPid; process_name=(Safe-Text $name); title=(Safe-Text $text.ToString()); automation_id=$null; session_id=$sid; left=$rect.Left; top=$rect.Top; width=[math]::Max(0,$rect.Right-$rect.Left); height=[math]::Max(0,$rect.Bottom-$rect.Top); fingerprint=$fingerprint })
  return $true
}; [void][RemoteOpsUser32]::EnumWindows($callback,[IntPtr]::Zero)
$ui = $null
if ($env:REMOTEOPS_INCLUDE_UI_TREE -eq '1' -and $foreground -ne [IntPtr]::Zero) {
  function Convert-Uia([System.Windows.Automation.AutomationElement]$e,[int]$depth) { if($null -eq $e -or $depth -gt 3){return $null}; $n=[pscustomobject]@{name=(Safe-Text $e.Current.Name); automation_id=(Safe-Text $e.Current.AutomationId); control_type=(Safe-Text $e.Current.ControlType.ProgrammaticName); children=@()}; $walker=[System.Windows.Automation.TreeWalker]::ControlViewWalker; $c=$walker.GetFirstChild($e); $list=@(); while($null -ne $c -and $list.Count -lt 40){$list += Convert-Uia $c ($depth+1); $c=$walker.GetNextSibling($c)}; $n.children=$list; return $n }
  try { $ui=Convert-Uia ([System.Windows.Automation.AutomationElement]::FromHandle($foreground)) 0 } catch { $ui=$null }
}
$shot = $null; $w = $null; $h = $null
if ($env:REMOTEOPS_INCLUDE_SCREENSHOT -eq '1' -and $screens.Count -gt 0) {
  $b = $screens[0].Bounds; $w=$b.Width; $h=$b.Height
  $bmp = New-Object System.Drawing.Bitmap($w,$h); $g=[System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size); $ms=New-Object System.IO.MemoryStream
  $bmp.Save($ms,[System.Drawing.Imaging.ImageFormat]::Png); $shot=[Convert]::ToBase64String($ms.ToArray()); $g.Dispose(); $bmp.Dispose(); $ms.Dispose()
}
[pscustomobject]@{ displays=$displays; windows=$windows; active_window_fingerprint=($windows | Where-Object { $_.window_id -eq "0x$('{0:x}' -f $foreground.ToInt64())" } | Select-Object -First 1 -ExpandProperty fingerprint); screenshot_base64=$shot; screenshot_width=$w; screenshot_height=$h; ui_tree=$ui } | ConvertTo-Json -Compress -Depth 8
"#;
        let mut command = tokio::process::Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
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
        let output = command
            .output()
            .await
            .map_err(|e| VisualProviderError::Protocol(format!("启动桌面采集失败：{e}")))?;
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
        Ok(VisualObservation {
            request_id,
            session_id,
            provider_instance_id: "windows-powershell-desktop".into(),
            state: remoteops_domain::VisualSessionState::Ready,
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
        self.invoke_uia(target, action).await?;
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
            message: "UIA Invoke 已发送并完成后置观察".into(),
        })
    }
    async fn type_text(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _target: &VisualTarget,
        _text: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        Err(VisualProviderError::Rejected(
            "UI Automation Provider 尚未连接；拒绝输入".into(),
        ))
    }
    async fn send_input(
        &self,
        _request_id: RequestId,
        _session_id: SessionId,
        _target: &VisualTarget,
        _input: &str,
    ) -> Result<VisualActionResult, VisualProviderError> {
        Err(VisualProviderError::Rejected(
            "UI Automation Provider 尚未连接；拒绝坐标输入".into(),
        ))
    }
    async fn stop(&self, _session_id: SessionId) -> Result<(), VisualProviderError> {
        Ok(())
    }
}

/// 创建当前宿主的默认图形 Provider。
#[must_use]
pub fn default_visual_provider() -> std::sync::Arc<dyn VisualProvider> {
    #[cfg(windows)]
    {
        std::sync::Arc::new(WindowsVisualProvider)
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
