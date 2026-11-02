# 必须先经过有声明类型的 C# 帧初始化代理，避免 PowerShell 动态栈帧触发 WPF 的空引用。
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName UIAutomationClientsideProviders
if (-not ('RemoteOpsUiaClientBootstrap' -as [type])) {
    $remoteOpsUiaReferences = @(
        [System.Windows.Automation.AutomationElement].Assembly.Location,
        [System.Windows.Automation.AutomationProperty].Assembly.Location,
        [UIAutomationClientsideProviders.UIAutomationClientSideProviders].Assembly.Location
    )
    Add-Type -ReferencedAssemblies $remoteOpsUiaReferences -TypeDefinition @'
/// <summary>为 PowerShell 客户端初始化标准控件的托管 UIA 代理。</summary>
public static class RemoteOpsUiaClientBootstrap
{
    // 每个观察或输入子进程只注册一次代理。
    private static bool initialized;

    /// <summary>在第一个 UIA 查询前加载代理，保留具有声明类型且不能内联的初始化栈帧。</summary>
    [System.Runtime.CompilerServices.MethodImpl(System.Runtime.CompilerServices.MethodImplOptions.NoInlining)]
    public static void Initialize()
    {
        if (initialized) return;
        System.Windows.Automation.ClientSettings.RegisterClientSideProviders(
            UIAutomationClientsideProviders.UIAutomationClientSideProviders.ClientSideProviderDescriptionTable);
        initialized = true;
    }
}
'@
}
[RemoteOpsUiaClientBootstrap]::Initialize()
