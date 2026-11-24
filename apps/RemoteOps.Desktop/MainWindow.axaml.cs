using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Layout;
using Avalonia.Threading;
using System.Text.Json;

namespace RemoteOps.Desktop;

public partial class MainWindow : Window
{
    private sealed record Device(string Id,string DeviceId,string Name,bool Online,bool Desktop,bool Writable)
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
    private Device? Selected => Devices.SelectedItem as Device;
    private string? preferred;
    private bool closing, updating;
    private Device[] roster = [];
    private static string S(JsonElement e,string name) => e.TryGetProperty(name,out var v) && v.ValueKind==JsonValueKind.String ? v.GetString()! : "";

    public MainWindow()
    {
        InitializeComponent();
        Title=$"RemoteOps · 人工控制端 v{Program.DisplayVersion}";
        Devices.SelectionChanged+=(_,_)=>{if(!updating){preferred=Selected?.Id; Update();}};
        Pair.Click+=(_,_)=>PairDevice();
        Code.KeyDown+=(_,e)=>{if(e.Key==Key.Enter){PairDevice();e.Handled=true;}};
        OpenDesktop.Click+=(_,_)=>OpenViewer();
        ToolsButton.Click+=(_,_)=>ToolsPanel.IsVisible=!ToolsPanel.IsVisible;
        Refresh.Click+=(_,_)=>Post(new {op="refresh"});
        Run.Click+=(_,_)=>{if(Selected is {} d && !string.IsNullOrWhiteSpace(Command.Text)){Post(new {op="shell",session=d.Id,command=Command.Text});LogPanel.IsExpanded=true;}};
        ListPorts.Click+=(_,_)=>{if(Selected is {} d){Post(new {op="ports",session=d.Id});LogPanel.IsExpanded=true;}};
        OpenPort.Click+=(_,_)=>{if(Selected is {} d && uint.TryParse(Baud.Text,out var baud) && baud>0){Post(new {op="openPort",session=d.Id,port=Port.Text,baud,writable=Writable.IsChecked==true});}else Status.Text="请输入有效波特率";};
        WritePort.Click+=(_,_)=>{if(Selected is {} d && serials.TryGetValue(d.Id,out var port) && !string.IsNullOrEmpty(SerialText.Text))Post(new {op="writePort",session=d.Id,serial=port,text=SerialText.Text+"\r"});};
        ClosePort.Click+=(_,_)=>{if(Selected is {} d && serials.TryGetValue(d.Id,out var port))Post(new {op="closePort",session=d.Id,serial=port});};
        Disconnect.Click+=async (_,_)=>{
            if(Selected is {} d && await Confirm("断开整个会话",$"结束与 {d.Name} 的连接？桌面共享和同一会话中的 MCP 操作都会结束。"))
                Post(new {op="disconnect",session=d.Id});
        };
        SettingsButton.Click+=async (_,_)=>{
            if(viewer!=null){Status.Text="请先结束桌面共享，再更改连接设置";return;}
            var result=await Profile.Edit(this,profile);
            if(result is not {} choice)return;
            try { choice.Item1.Save(); profile=choice.Item1; credential=choice.Item2; Start(); }
            catch(Exception ex){Status.Text=ex.Message;}
        };
        Opened+=(_,_)=>{try{profile=Profile.Load();Start();}catch(Exception e){Status.Text=e.Message;}};
        pump.Tick+=(_,_)=>Drain();
        Closing+=(_,_)=>{closing=true;viewer?.Close();pump.Stop();core?.Dispose();core=null;};
        Update();
    }

    private void Start()
    {
        pump.Stop();core?.Dispose();core=null;
        approvals.Clear();serials.Clear();logs.Clear();SetDevices([]);Update();
        Pair.IsEnabled=true;Status.Text="正在连接 Relay";
        try {core=new DesktopCore(new {profile.relay,profile.server,profile.ca,profile.owner,token=credential});pump.Start();}
        catch(Exception e){Status.Text="无法载入人工会话组件 · "+e.Message;}
    }
    private void PairDevice()
    {
        if(!Pair.IsEnabled)return;
        if(string.IsNullOrWhiteSpace(Code.Text)){Status.Text="请输入现场控制码";return;}
        if(Post(new {op="pair",code=Code.Text.Trim()})){Pair.IsEnabled=false;Status.Text="正在连接";}
    }
    private bool Post(object command)
    {
        try {if(core==null)throw new InvalidOperationException("请先完成连接设置");core.Post(command);return true;}
        catch(Exception e){Status.Text=e.Message;return false;}
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
        next.Show();
    }
    private static Device ReadDevice(JsonElement e) =>
        new(S(e,"session"),S(e,"device_id"),S(e,"name"),e.GetProperty("online").GetBoolean(),
            e.GetProperty("desktop").GetBoolean(),e.GetProperty("writable").GetBoolean());
    private void Paired(JsonElement e)
    {
        var d=ReadDevice(e.GetProperty("device"));
        Pair.IsEnabled=true; Code.Clear(); preferred=d.Id;
        SetDevices(roster.Where(item=>item.Id!=d.Id).Append(d).ToArray());
        Status.Text=d.Desktop?"已连接，正在打开观察画面":"已连接，设备未提供实时桌面";
        if(d.Online && d.Desktop)OpenViewer(d);
    }
    private void SetDevices(Device[] items)
    {
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
    private void Update()
    {
        var d=Selected;
        Target.Text=d?.Name??"尚未选择设备";
        OpenDesktop.IsEnabled=d is {Online:true,Desktop:true};
        ToolsButton.IsEnabled=Disconnect.IsEnabled=d?.Online==true;
        ToolsPanel.IsEnabled=d?.Online==true;
        bool serial=d!=null && serials.ContainsKey(d.Id);
        WritePort.IsEnabled=ClosePort.IsEnabled=serial; OpenPort.IsEnabled=!serial;
        Log.Text=d!=null?logs.GetValueOrDefault(d.Id,""):"";
        Pending.Children.Clear();
        foreach(var a in approvals.Values.Where(a=>a.Device==d?.Id)){
            var buttons=new StackPanel {Orientation=Orientation.Horizontal,Spacing=8};
            foreach(var accept in new[]{true,false}){
                var button=new Button {Content=accept?"批准":"拒绝"};
                button.Click+=(_,_)=>{if(Post(new {op="decide",session=a.Device,id=a.Id,approved=accept}))buttons.IsEnabled=false;};
                buttons.Children.Add(button);
            }
            Pending.Children.Add(new TextBlock {Text=a.Text,TextWrapping=Avalonia.Media.TextWrapping.Wrap});
            Pending.Children.Add(buttons);
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
                case "devices": SetDevices(e.GetProperty("items").EnumerateArray().Select(ReadDevice).ToArray()); break;
                case "paired": Paired(e);break;
                case "error":
                    if(S(e,"op")=="pair")Pair.IsEnabled=true;
                    if(S(e,"op")=="open" && viewer?.Session==id)viewer.OpenFailed(e,text);
                    Status.Text=text;Append(id,text);break;
                case "clipboard_support":
                    if(viewer?.Session==id && viewer.Matches(e))viewer.SetClipboardSupport(e.GetProperty("available").GetBoolean());
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
                default: Status.Text=text;Append(id,text);break;
            }
        }
    }
    private Task<bool> Confirm(string title,string question)
    {
        var dialog=new Window{Title=title,Width=410,SizeToContent=SizeToContent.Height,WindowStartupLocation=WindowStartupLocation.CenterOwner,CanResize=false};
        var box=new StackPanel{Margin=new Thickness(22),Spacing=16};
        box.Children.Add(new TextBlock{Text=question,TextWrapping=Avalonia.Media.TextWrapping.Wrap});
        var actions=new StackPanel{Orientation=Orientation.Horizontal,HorizontalAlignment=HorizontalAlignment.Right,Spacing=8};
        foreach(var confirm in new[]{false,true}){var b=new Button{Content=confirm?title:"取消"};b.Click+=(_,_)=>dialog.Close(confirm);actions.Children.Add(b);}
        box.Children.Add(actions);dialog.Content=box;return dialog.ShowDialog<bool>(this);
    }
}
