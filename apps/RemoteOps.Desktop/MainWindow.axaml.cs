using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Layout;
using Avalonia.Media;
using Avalonia.Threading;
using System.Text.Json;

namespace RemoteOps.Desktop;

public partial class MainWindow : Window
{
    private sealed record Device(string Id,string DeviceId,string Name,string Host,string? Code,bool Online,bool Desktop,bool Writable)
    {
        public override string ToString() => $"{Name} · {(Online ? "在线" : "离线")}";
    }
    private sealed record Approval(string Device,string Id,string Text);
    private DesktopCore? core;
    private ViewerWindow? viewer;
    private Profile profile = new();
    private string? credential;
    private readonly DispatcherTimer pump = new() { Interval=TimeSpan.FromMilliseconds(45) };
    private readonly Dictionary<string,string> logs = new(), serials = new();
    private readonly Dictionary<string,Approval> approvals = new();
    private readonly Dictionary<(string Session,string Device),string> pairingCodes = new();
    private Device? Selected => Devices.SelectedItem as Device;
    private string? preferred;
    private bool closing, updating, relayReady, pairing;
    private Device[] roster = [];
    private static string S(JsonElement e,string name) => e.TryGetProperty(name,out var v) && v.ValueKind==JsonValueKind.String ? v.GetString()! : "";

    /// <summary>初始化控制端主窗口和本机外观选项。</summary>
    public MainWindow()
    {
        InitializeComponent();
        WindowPlacement.CenterOnOpen(this);
        Title=$"RemoteOps · 远程运维 v{Program.DisplayVersion}";
        ConfigureAppearance();
        Devices.SelectionChanged+=(_,_)=>{if(!updating){preferred=Selected?.Id; Update();}};
        SettingsButton.IsVisible=!Profile.IsManaged;
        RetryRelay.Click+=(_,_)=>Start();
        Pair.Click+=(_,_)=>PairDevice();
        Code.TextChanged+=(_,_)=>UpdatePairAvailability();
        Code.KeyDown+=(_,e)=>{if(e.Key==Key.Enter){PairDevice();e.Handled=true;}};
        OpenDesktop.Click+=(_,_)=>OpenViewer();
        ToolsButton.Click+=(_,_)=>ToolsPanel.IsVisible=!ToolsPanel.IsVisible;
        Refresh.Click+=(_,_)=>Post(new {op="refresh"});
        Run.Click+=(_,_)=>{if(Selected is {} d && !string.IsNullOrWhiteSpace(Command.Text)){Post(new {op="shell",session=d.Id,command=Command.Text});LogPanel.IsExpanded=true;}};
        ListPorts.Click+=(_,_)=>{if(Selected is {} d){Post(new {op="ports",session=d.Id});LogPanel.IsExpanded=true;}};
        OpenPort.Click+=(_,_)=>{if(Selected is {} d && uint.TryParse(Baud.Text,out var baud) && baud>0){Post(new {op="openPort",session=d.Id,port=Port.Text,baud,writable=Writable.IsChecked==true});}else SetStatus("请输入有效波特率");};
        WritePort.Click+=(_,_)=>{if(Selected is {} d && serials.TryGetValue(d.Id,out var port) && !string.IsNullOrEmpty(SerialText.Text))Post(new {op="writePort",session=d.Id,serial=port,text=SerialText.Text+"\r"});};
        ClosePort.Click+=(_,_)=>{if(Selected is {} d && serials.TryGetValue(d.Id,out var port))Post(new {op="closePort",session=d.Id,serial=port});};
        CopyPairCode.Click+=async (_,_)=>{
            if(Selected?.Code is {} code && Clipboard is {} clipboard)
            {
                try { await clipboard.SetTextAsync(code); SetStatus("已复制本次配对码"); }
                catch(Exception error) { SetStatus("复制失败 · "+error.Message); }
            }
        };
        Disconnect.Click+=async (_,_)=>{
            if(Selected is {} d && await Confirm("断开本次人工运维",$"结束对 {d.Name} 的人工桌面和工具连接？AI 工程师可以继续工作。"))
                Post(new {op="disconnect",session=d.Id});
        };
        SettingsButton.Click+=async (_,_)=>{
            if(viewer!=null){SetStatus("请先结束桌面共享，再更改连接设置");return;}
            var result=await Profile.Edit(this,profile);
            if(result is not {} choice)return;
            try { choice.Item1.Save(); profile=choice.Item1; credential=choice.Item2; Start(); }
            catch(Exception ex){SetStatus(ex.Message);}
        };
        Opened+=(_,_)=>{
            try {
                profile=Profile.Load();
                if(Profile.IsManaged){
                    ProductName.Text=profile.productName;
                    Title=$"{profile.productName} · 远程运维 v{Program.DisplayVersion}";
                }
                Code.Focus();
                Start();
            } catch(Exception e){
                SetServiceStatus("服务未连接");
                SetStatus(e.Message);
            }
        };
        pump.Tick+=(_,_)=>Drain();
        Closing+=(_,_)=>{closing=true;viewer?.Close();pump.Stop();core?.Dispose();core=null;};
        Update();
    }

    private void ConfigureAppearance()
    {
        var options = new[] { (AppearanceAuto, "auto"), (AppearanceLight, "light"), (AppearanceDark, "dark") };
        foreach (var (option, theme) in options)
        {
            option.Click += (_, _) =>
            {
                var saved = DesktopPreferences.SetTheme(theme);
                ToolTip.SetTip(AppearancePicker, saved ? null : "本次已切换，偏好写入失败");
            };
        }

        UpdateSelection();
        if (Application.Current is not {} application) return;
        application.PropertyChanged += ThemeChanged;
        Closed += (_, _) => application.PropertyChanged -= ThemeChanged;

        void ThemeChanged(object? sender, AvaloniaPropertyChangedEventArgs change)
        {
            if (change.Property == Application.RequestedThemeVariantProperty) UpdateSelection();
        }

        void UpdateSelection()
        {
            var theme = DesktopPreferences.RequestedTheme;
            foreach (var (option, key) in options)
                option.IsChecked = key == theme;
        }
    }

    private void SetServiceStatus(string text, bool online = false)
    {
        ServiceStatus.Text = text;
        ServiceBadge.Classes.Set("state-online", online);
        ServiceBadge.Classes.Set("state-offline", !online);
    }

    private void SetStatus(string? text)
    {
        Status.Text = text;
        Status.IsVisible = !string.IsNullOrWhiteSpace(text);
    }

    private void UpdatePairAvailability() =>
        Pair.IsEnabled = relayReady && !pairing && !string.IsNullOrWhiteSpace(Code.Text);

    private void Start()
    {
        pump.Stop();core?.Dispose();core=null;
        approvals.Clear();serials.Clear();logs.Clear();pairingCodes.Clear();SetDevices([]);Update();
        relayReady=false; pairing=false; UpdatePairAvailability(); Refresh.IsEnabled=false; RetryRelay.IsVisible=false;
        SetServiceStatus("正在连接服务");
        SetStatus(null);
        try {core=new DesktopCore(new {profile.relay,profile.server,profile.ca,profile.owner,token=credential,version=Program.DisplayVersion,managed=Profile.IsManaged});pump.Start();}
        catch(Exception e){
            RetryRelay.IsVisible=true;
            SetServiceStatus("服务未连接");
            SetStatus("无法载入人工会话组件 · "+e.Message);
        }
    }
    private void PairDevice()
    {
        if(!Pair.IsEnabled || Code.Text?.Trim() is not { Length: > 0 } code)return;
        if(Post(new {op="pair",code})){pairing=true;UpdatePairAvailability();SetStatus("正在配对");}
    }
    private bool Post(object command)
    {
        try {if(core==null)throw new InvalidOperationException("请先完成连接设置");core.Post(command);return true;}
        catch(Exception e){SetStatus(e.Message);return false;}
    }
    private void OpenViewer()
    {
        if(Selected is {} device) OpenViewer(device);
    }
    private void OpenViewer(Device d)
    {
        if(!d.Online || !d.Desktop || core==null)return;
        if(viewer?.Session==d.Id){viewer.RestoreAndActivate();return;}
        viewer?.Close();
        var next=new ViewerWindow(core,d.Id,d.Name,d.Writable,d.DeviceId);
        viewer=next;
        next.Closed+=(_,_)=>{if(viewer==next)viewer=null;};
        WindowPlacement.CenterOnOpen(next,this);
        next.Show();
    }
    private Device ReadDevice(JsonElement e)
    {
        var session=S(e,"session");
        var device=S(e,"device_id");
        var host=S(e,"hostname");
        var code=pairingCodes.GetValueOrDefault((session,device));
        return new(session,device,code??(host.Length>0?host:S(e,"name")),host,code,
            e.GetProperty("online").GetBoolean(),e.GetProperty("desktop").GetBoolean(),e.GetProperty("writable").GetBoolean());
    }
    private void Paired(JsonElement e)
    {
        var descriptor=e.GetProperty("device");
        if(S(e,"pair_code") is {Length:>0} code)
            pairingCodes[(S(descriptor,"session"),S(descriptor,"device_id"))]=code;
        var d=ReadDevice(descriptor);
        pairing=false; Code.Clear(); UpdatePairAvailability(); preferred=d.Id;
        SetDevices(roster.Where(item=>item.Id!=d.Id).Append(d).ToArray());
        SetStatus(d.Desktop?"已连接，正在打开观察画面":"已连接，设备未提供实时桌面");
        if(d.Online && d.Desktop)OpenViewer(d);
    }
    private void SetDevices(Device[] items)
    {
        foreach(var key in pairingCodes.Keys.ToArray())
            if(!items.Any(item=>item.Id==key.Session && item.DeviceId==key.Device))pairingCodes.Remove(key);
        roster=items;var selected=preferred??Selected?.Id;
        if(viewer is {} active && items.FirstOrDefault(item=>item.Id==active.Session) is {} current)
            active.SetWritable(current.Online && current.Writable);
        updating=true;
        Devices.ItemsSource=items;
        Devices.SelectedItem=items.FirstOrDefault(d=>d.Id==selected)??items.FirstOrDefault();
        updating=false;
        if(preferred==null || items.Any(d=>d.Id==preferred))preferred=Selected?.Id;
        Update();
    }
    public static string DeviceStateBadgeLabel(bool online, bool desktop, bool writable)
    {
        if (!online) return "离线";
        if (!desktop) return "在线 · 不支持实时桌面";
        return writable ? "在线 · 可控制" : "在线 · 仅观察";
    }

    private void Update()
    {
        var d=Selected;
        DeviceCount.Text=roster.Length.ToString();
        EmptyDevicesHint.IsVisible=roster.Length==0;
        Devices.IsVisible=roster.Length!=0;
        DeviceWorkspace.IsVisible=d!=null;
        Target.Text=d?.Name;
        DeviceCaption.Text=d?.Code!=null?"本次配对码"+(string.IsNullOrEmpty(d.Host)?"":" · "+d.Host):"设备";
        CopyPairCode.IsVisible=d?.Code!=null;
        TargetStateBadge.IsVisible=d!=null;
        if(d!=null)
        {
            TargetStateText.Text=DeviceStateBadgeLabel(d.Online,d.Desktop,d.Writable);
            TargetStateBadge.Classes.Set("state-online",d.Online && d.Desktop);
            TargetStateBadge.Classes.Set("state-online-limited",d.Online && !d.Desktop);
            TargetStateBadge.Classes.Set("state-offline",!d.Online);
        }
        OpenDesktop.IsEnabled=d is {Online:true,Desktop:true};
        ToolsButton.IsEnabled=Disconnect.IsEnabled=d?.Online==true;
        ToolsPanel.IsEnabled=d?.Online==true;
        bool serial=d!=null && serials.ContainsKey(d.Id);
        WritePort.IsEnabled=ClosePort.IsEnabled=serial; OpenPort.IsEnabled=!serial;
        Log.Text=d!=null?logs.GetValueOrDefault(d.Id,""):"";
        Pending.Children.Clear();
        foreach(var a in approvals.Values.Where(a=>a.Device==d?.Id)){
            var card=new Border{Classes={"warn-card"}};
            var stack=new StackPanel{Spacing=10};
            stack.Children.Add(new TextBlock{Text="待审批操作请求",FontSize=12.5,FontWeight=FontWeight.SemiBold,Classes={"muted"}});
            stack.Children.Add(new TextBlock{Text=a.Text,TextWrapping=TextWrapping.Wrap});
            var buttons=new StackPanel{Orientation=Orientation.Horizontal,Spacing=8};
            var approve=new Button{Content="批准执行",Classes={"primary"}};
            approve.Click+=(_,_)=>{if(Post(new {op="decide",session=a.Device,id=a.Id,approved=true}))buttons.IsEnabled=false;};
            var reject=new Button{Content="拒绝",Classes={"end"}};
            reject.Click+=(_,_)=>{if(Post(new {op="decide",session=a.Device,id=a.Id,approved=false}))buttons.IsEnabled=false;};
            buttons.Children.Add(approve);
            buttons.Children.Add(reject);
            stack.Children.Add(buttons);
            card.Child=stack;
            Pending.Children.Add(card);
        }
    }
    private void Append(string id,string text)
    {
        if(string.IsNullOrEmpty(id)||string.IsNullOrWhiteSpace(text))return;
        var entry=logs.GetValueOrDefault(id,"") + $"[{DateTime.Now:HH:mm:ss}] {text.TrimEnd()}" + Environment.NewLine;
        logs[id]=entry.Length>20000?entry[^18000..]:entry;
        if(Selected?.Id==id)Log.Text=logs[id];
    }
    private void Drain()
    {
        if(core==null || closing)return;
        for(int i=0;i<80;i++){
            var json=core.Poll();if(json==null)return;
            using var doc=JsonDocument.Parse(json);var e=doc.RootElement;
            var id=S(e,"session");var text=S(e,"text");
            switch(S(e,"kind")){
                case "connection":
                    relayReady=e.GetProperty("online").GetBoolean();
                    if(!relayReady)pairing=false;
                    UpdatePairAvailability();
                    Refresh.IsEnabled=relayReady;
                    var canRetry=!relayReady && e.TryGetProperty("retry",out var retry) && retry.GetBoolean();
                    RetryRelay.IsVisible=canRetry;
                    var update=S(e,"update");
                    SetServiceStatus(relayReady && update.Length>0 ? $"可更新至 v{update}" : relayReady?"服务已连接":canRetry?"服务未连接":"正在重新连接服务",relayReady);
                    SetStatus(canRetry?text:null);
                    break;
                case "devices": SetDevices(e.GetProperty("items").EnumerateArray().Select(ReadDevice).ToArray()); break;
                case "paired": Paired(e);break;
                case "error":
                    if(S(e,"op")=="pair"){pairing=false;UpdatePairAvailability();}
                    if(S(e,"op")=="open" && viewer?.Session==id)viewer.OpenFailed(e,text);
                    SetStatus(text);Append(id,text);break;
                case "clipboard_support":
                    if(viewer?.Session==id && viewer.Matches(e))viewer.SetClipboardSupport(e.GetProperty("available").GetBoolean());
                    break;
                case "video_stats":
                    if(viewer?.Session==id && viewer.Matches(e))viewer.SetStatistics(e);
                    break;
                case "route_detail":
                    if(viewer?.Session==id && viewer.Matches(e))Append(id,text);
                    break;
                case "route":
                    if(viewer?.Session==id && viewer.Matches(e)){viewer.SetRoute(e,text);Append(id,text);}
                    break;
                case "desktop": if(viewer?.Session==id)viewer.State(e); Append(id,text);break;
                case "approval": approvals[S(e,"id")]=new(id,S(e,"id"),text);Append(id,text);Update();break;
                case "decided": approvals.Remove(S(e,"id"));Append(id,text);Update();break;
                case "ports":
                    if(e.TryGetProperty("items",out var ports) && ports.ValueKind==JsonValueKind.Object && ports.TryGetProperty("ports",out var list))
                        text=string.Join(Environment.NewLine,list.EnumerateArray().Select(p=>S(p,"port_name") is {Length:>0} n?n:p.ToString()));
                    Append(id,text);break;
                case "serial": var port=S(e,"serial");if(port.Length==0)serials.Remove(id);else serials[id]=port;Append(id,text);Update();break;
                case "output": Append(id,text);break;
                default: SetStatus(text);Append(id,text);break;
            }
        }
    }
    private Task<bool> Confirm(string title,string question)
    {
        var dialog=new Window
        {
            Title=title,
            Width=440,
            SizeToContent=SizeToContent.Height,
            WindowStartupLocation=WindowStartupLocation.CenterOwner,
            CanResize=false
        };
        var layout=new Grid{RowDefinitions=new RowDefinitions("*,Auto")};
        var body=new StackPanel{Margin=new Thickness(24,22,24,18),Spacing=10};
        body.Children.Add(new TextBlock{Text=title,FontSize=18,FontWeight=FontWeight.SemiBold});
        body.Children.Add(new TextBlock{Text=question,Classes={"muted"},TextWrapping=TextWrapping.Wrap,LineHeight=22});
        layout.Children.Add(body);

        var footer=new Border{Classes={"dialog-footer"}};
        Grid.SetRow(footer,1);
        var actions=new StackPanel{Orientation=Orientation.Horizontal,HorizontalAlignment=HorizontalAlignment.Right,Spacing=10};
        var cancel=new Button{Content="取消"};
        cancel.Click+=(_,_)=>dialog.Close(false);
        var ok=new Button{Content=title,Classes={"end"}};
        ok.Click+=(_,_)=>dialog.Close(true);
        actions.Children.Add(cancel);
        actions.Children.Add(ok);
        footer.Child=actions;
        layout.Children.Add(footer);

        dialog.Content=layout;
        return dialog.ShowDialog<bool>(this);
    }
}
