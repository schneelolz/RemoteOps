using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text.Json;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;
using Avalonia.Media;
using Avalonia.Threading;
using RemoteOps.Desktop;

/// <summary>用真实人工窗口和同机测试 Agent 测量按键到合成桌面变色的耗时。</summary>
internal static class RelayLatency
{
    [StructLayout(LayoutKind.Sequential)]
    private struct Msg { public nint window; public uint message; public nuint wp; public nint lp; public uint time; public int x,y; public uint reserved; }
    [DllImport("user32")] private static extern bool RegisterHotKey(nint h,int id,uint mods,uint key);
    [DllImport("user32")] private static extern bool UnregisterHotKey(nint h,int id);
    [DllImport("user32")] private static extern bool PeekMessageW(out Msg message,nint h,uint first,uint last,uint remove);
    [DllImport("user32")] private static extern nint GetForegroundWindow();
    [DllImport("user32")] private static extern bool GetCursorPos(out PixelPoint position);
    [DllImport("user32")] private static extern bool SetCursorPos(int x,int y);
    [DllImport("user32")] private static extern void mouse_event(uint flags,uint x,uint y,uint data,nuint extra);
    private static bool ActivateTarget(Window target, Border mark)
    {
        var hwnd=target.TryGetPlatformHandle()!.Handle;
        if(GetForegroundWindow()==hwnd)return true;
        var pt=mark.PointToScreen(new Point(mark.Bounds.Width/2,mark.Bounds.Height/2));
        if(GetAncestor(WindowFromPoint(pt),2)!=hwnd || !GetCursorPos(out var prior))return false;
        if(!SetCursorPos(pt.X,pt.Y))return false;
        try {
            if(GetAncestor(WindowFromPoint(pt),2)!=hwnd)return false;
            mouse_event(2,0,0,0,0);mouse_event(4,0,0,0,0);
            return true;
        } finally {SetCursorPos(prior.X,prior.Y);}
    }
    [DllImport("user32")] private static extern nint WindowFromPoint(PixelPoint point);
    [DllImport("user32")] private static extern nint GetAncestor(nint hwnd,uint type);
    [DllImport("user32")] private static extern nint GetDC(nint hwnd);
    [DllImport("user32")] private static extern int ReleaseDC(nint hwnd,nint dc);
    [DllImport("gdi32")] private static extern uint GetPixel(nint dc,int x,int y);
    [StructLayout(LayoutKind.Sequential)]
    private struct ProcessInfo { public nint reserved,peb,reserved2,reserved3,pid,parent; }
    [DllImport("ntdll")] private static extern int NtQueryInformationProcess(nint process,int type,out ProcessInfo info,int size,out int used);

    private static void InterruptOwnMediaWorker(string backend)
    {
        var name = backend switch {
            "crossdesk" => "remoteops-crossdesk",
            "minirtc" => "remoteops-minirtc",
            _ => throw new ArgumentOutOfRangeException(nameof(backend))
        };
        string expected=Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, name + ".exe"));
        foreach(var p in Process.GetProcessesByName(name))
        using(p) {
            if(NtQueryInformationProcess(p.Handle,0,out var info,Marshal.SizeOf<ProcessInfo>(),out _)!=0 ||
                info.parent!=Environment.ProcessId || !string.Equals(p.MainModule?.FileName,expected,StringComparison.OrdinalIgnoreCase))continue;
            p.Kill();return;
        }
        throw new InvalidOperationException("找不到本次测试进程的媒体组件");
    }
    private static readonly Color Green = Color.FromRgb(20, 225, 55);
    private static readonly Color Pink = Color.FromRgb(225, 22, 195);

    internal static int Run(string file, bool fieldTarget = false)
    {
        var code = Console.ReadLine();
        if (string.IsNullOrWhiteSpace(code)) return 2;
        AppBuilder.Configure<App>().UsePlatformDetect().SetupWithoutStarting();
        using var stop = new CancellationTokenSource();
        var result = 1;
        Dispatcher.UIThread.Post(async () => {
            try {
                if (fieldTarget) await FieldRelayLatency.Measure(code, file);
                else await Measure(code, file);
                result = 0;
            }
            catch (Exception error) { Console.Error.WriteLine(error.GetType().Name + " · " + error.Message); }
            finally { stop.Cancel(); }
        });
        Dispatcher.UIThread.MainLoop(stop.Token);
        return result;
    }

    internal static async Task Wait(Func<bool> check,string description,int milliseconds=18000)
    {
        var clock=Stopwatch.StartNew();
        while(!check()) {
            if(clock.ElapsedMilliseconds>milliseconds) throw new TimeoutException(description);
            await Task.Delay(8);
        }
    }

    private static async Task Measure(string code, string file)
    {
        ViewerWindow? viewer=null;
        using var opened=Window.WindowOpenedEvent.AddClassHandler<ViewerWindow>((w,_)=>viewer=w);
        var main=new MainWindow();
        var mark=new Border { Width=128, Height=96, Background=new SolidColorBrush(Green) };
        var board=new Canvas { Background=new SolidColorBrush(Color.FromRgb(47,52,61)) };
        Canvas.SetLeft(mark,12); Canvas.SetTop(mark,14); board.Children.Add(mark);
        var target=new Window { Title="RemoteOps 延迟测试目标", ShowInTaskbar=false,
            SystemDecorations=SystemDecorations.None, Content=board };
        int keyCount=0;
        // 用保留的测试热键接收 Agent 注入的系统输入，避免键入现场的前台应用。
        using var finish=new CancellationTokenSource();
        using var ready=new ManualResetEventSlim();
        bool registered=false;
        var listener=new Thread(()=>{
            registered=RegisterHotKey(0,1,0x4000,0x86);
            ready.Set();
            if(!registered)return;
            try {
                while(!finish.IsCancellationRequested) {
                    if(PeekMessageW(out _,0,0x0312,0x0312,1))
                        Dispatcher.UIThread.Post(()=>{++keyCount;mark.Background=new SolidColorBrush((keyCount&1)==0?Green:Pink);});
                    else Thread.Sleep(1);
                }
            } finally {UnregisterHotKey(0,1);}
        });
        listener.Start();ready.Wait();
        if(!registered)throw new InvalidOperationException("测试热键 F23 被占用");
        var dc=GetDC(0);
        try {
            main.Show();
            main.FindControl<TextBox>("Code")!.Text=code;
            var pair=main.FindControl<Button>("Pair")!;
            await Wait(()=>pair.IsEnabled,"连接测试 Relay");
            pair.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            try { await Wait(()=>viewer?.FindControl<Image>("Screen")?.Source!=null,"配对并接收首帧",35000); }
            catch(TimeoutException) {
                var mainStatus=main.FindControl<TextBlock>("Status")?.Text;
                var viewerStatus=viewer?.FindControl<TextBlock>("Mode")?.Text;
                throw new TimeoutException($"配对或首帧超时 · {mainStatus} · {viewerStatus}");
            }
            var v=viewer!;
            var screen=v.Screens.Primary ?? throw new InvalidOperationException("无活动主显示器");
            var primary=screen.Bounds;
            var secondary=v.Screens.All.FirstOrDefault(s=>!s.IsPrimary);
            target.Width=secondary==null?160:primary.Width/screen.Scaling;
            target.Height=secondary==null?130:primary.Height/screen.Scaling;
            target.Topmost=secondary==null;
            target.Show();
            target.Position=secondary==null?new PixelPoint(primary.X+20,primary.Y+180):primary.Position;

            var display=secondary?.WorkingArea ?? primary;
            v.Width=Math.Min(1100,display.Width*.72/screen.Scaling);
            v.Height=Math.Min(740,display.Height*.78/(screen.Scaling));
            v.Position=new PixelPoint(display.X+display.Width-(int)(v.Width*(screen.Scaling))-8,display.Y+Math.Max(180,(display.Height-(int)(v.Height*screen.Scaling))/2));
            // 先移至停止面板下方，再置顶，确保实测像素来自运维端。
            v.Topmost=true;
            var canvas=v.FindControl<Grid>("CanvasArea")!;
            var image=v.FindControl<Image>("Screen")!;
            var control=v.FindControl<Button>("Control")!;
            var mode=v.FindControl<TextBlock>("Mode")!;
            var route=v.FindControl<TextBlock>("Route")!;
            v.FindControl<ToggleSwitch>("ClipboardShare")!.IsChecked=false;
            string lastDisplay="";
            var diagnosticTimer=new DispatcherTimer{Interval=TimeSpan.FromMilliseconds(250)};
            diagnosticTimer.Tick+=(_,_)=>{var status=mode.Text+" · "+route.Text; if(status!=lastDisplay){lastDisplay=status;Console.WriteLine(status);}};
            diagnosticTimer.Start();
            var menu=(MenuFlyout)v.FindControl<Button>("PictureQuality")!.Flyout!;
            menu.Items.OfType<MenuItem>().Single(i=>i.Tag?.ToString()=="balanced")
                .RaiseEvent(new RoutedEventArgs(MenuItem.ClickEvent));
            var submenu=menu.Items.OfType<MenuItem>().Single(i=>i.Header?.ToString()=="传输方式");
            var rows=new List<object>();
            var measurements=new Dictionary<string,List<double>>();
            var failures=new Dictionary<string,int>();
            static double Percentile(List<double> elapsed,double percentile) =>
                Math.Round(elapsed.Order().ElementAt((int)Math.Ceiling(elapsed.Count*percentile)-1),1);
            var startCode=Stopwatch.StartNew();
            bool fallbackVerified=false;
            int blocks=0;
            var compareCrossDesk=Environment.GetEnvironmentVariable("REMOTEOPS_BENCH_CROSSDESK")=="1";
            var transports=compareCrossDesk
                ? new[]{"tcp","minirtc","crossdesk","crossdesk","minirtc","tcp"}
                : new[]{"tcp","minirtc","minirtc","tcp"};
            var requested = Environment.GetEnvironmentVariable("REMOTEOPS_BENCH_ROUTES");
            if (!string.IsNullOrWhiteSpace(requested)) {
                transports = requested.Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
                if (transports.Length == 0 || transports.Any(t => t is not ("tcp" or "minirtc" or "crossdesk")))
                    throw new ArgumentException("延迟测试路线无效");
            }
            foreach(var transport in transports) {
                if(!measurements.ContainsKey(transport)) { measurements[transport]=new(); failures[transport]=0; }
                submenu.Items.OfType<MenuItem>().Single(i=>i.Tag?.ToString()==transport)
                    .RaiseEvent(new RoutedEventArgs(MenuItem.ClickEvent));
                bool CorrectRoute()=>transport switch {
                    "crossdesk" => route.Text=="CrossDesk · TURN 中继",
                    "minirtc" => route.Text=="MiniRTC · TURN/UDP 中继",
                    _ => route.Text=="TCP 中继"
                };
                try { await Wait(()=>CorrectRoute() && control.IsEnabled && image.Source!=null,"等待指定传输和首帧",25000); }
                catch (TimeoutException) {
                    ++failures[transport];
                    Save();
                    throw new TimeoutException($"等待 {transport}，路由 {route.Text}，状态 {mode.Text}");
                }
                if(mode.Text!="控制模式")control.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                await Wait(()=>mode.Text=="控制模式","等待人工接管");
                if(!ActivateTarget(target,mark))throw new InvalidOperationException("测试色块被遮挡");
                await Wait(()=>GetForegroundWindow()==target.TryGetPlatformHandle()!.Handle,"验证按键目标",2000);
                await Task.Delay(350);
                string diagnostic="";
                bool Visible(bool pink) {
                    var marker=mark.PointToScreen(new Point(mark.Bounds.Width/2,mark.Bounds.Height/2));
                    var w=image.Bounds.Width; var h=image.Bounds.Height;
                    var scale=Math.Min(w/primary.Width,h/primary.Height);
                    var sample=image.PointToScreen(new Point((w-primary.Width*scale)/2+(marker.X-primary.X)*scale,
                        (h-primary.Height*scale)/2+(marker.Y-primary.Y)*scale));
                    uint value=GetPixel(dc,sample.X,sample.Y);
                    diagnostic=$"状态 {mode.Text} / {route.Text}；源 {marker} {GetPixel(dc,marker.X,marker.Y):x6}，运维像素 {sample} {value:x6}，主屏 {primary}，图像 {image.Bounds}，真实帧 {image.Source?.Size}，前景覆盖 {GetAncestor(WindowFromPoint(sample),2)==v.TryGetPlatformHandle()!.Handle}";
                    diagnostic += " " + v.FindControl<TextBlock>("PerformanceText")?.Text;
                    var r=(int)(value&255); var g=(int)(value>>8&255); var b=(int)(value>>16&255);
                    if (GetAncestor(WindowFromPoint(sample), 2) != v.TryGetPlatformHandle()!.Handle) return false;
                    return pink ? r>140 && b>120 && g<100 : g>140 && r<100 && b<120;
                }
                try { await Wait(()=>Visible((keyCount&1)!=0),"在运维窗口定位测试色块"); }
                catch(TimeoutException) {throw new TimeoutException("定位色块 · "+diagnostic);}
                for(int sample=0;sample<(compareCrossDesk ? 25 : 13);sample++) {
                    if(GetForegroundWindow()!=target.TryGetPlatformHandle()!.Handle || !CorrectRoute() || mode.Text!="控制模式")
                        throw new InvalidOperationException("测试失去焦点或路由改变，已停止发送输入");
                    var before=keyCount; var t=Stopwatch.GetTimestamp();
                    canvas.RaiseEvent(new KeyEventArgs{RoutedEvent=InputElement.KeyDownEvent, Key=Key.F23,Source=canvas});
                    canvas.RaiseEvent(new KeyEventArgs{RoutedEvent=InputElement.KeyUpEvent, Key=Key.F23,Source=canvas});
                    var pink=(before&1)==0;
                    try {await Wait(()=>keyCount==before+1 && Visible(pink),"按键未到达测试窗口或未回显",6000);}
                    catch(TimeoutException) {++failures[transport]; Save(); throw new TimeoutException($"按键序号 {before}→{keyCount}，模式 {mode.Text}，前台为测试目标或运维端 {GetForegroundWindow()==target.TryGetPlatformHandle()!.Handle || GetForegroundWindow()==v.TryGetPlatformHandle()!.Handle}，{diagnostic}");}
                    var elapsed=Stopwatch.GetElapsedTime(t).TotalMilliseconds;
                    if(sample>=1) {
                        measurements[transport].Add(elapsed);
                        rows.Add(new {backend=transport,encoded=v.FindControl<TextBlock>("Resolution")!.Text,ms=Math.Round(elapsed,1)});
                    }
                    await Task.Delay(150+sample*13%90);
                }
                ++blocks;
                Save();
                Console.WriteLine($"完成 {transport} 一组");
                var fallback = Environment.GetEnvironmentVariable("REMOTEOPS_BENCH_FALLBACK");
                if ((!compareCrossDesk && blocks == 3 && fallback == "1") ||
                    (blocks == transports.Length && transport != "tcp" && fallback == transport)) {
                    InterruptOwnMediaWorker(transport);
                    await Wait(()=>route.Text?.StartsWith("TCP 中继")==true && mode.Text=="控制模式","等待 TCP 回退",9000);
                    if(GetForegroundWindow()!=target.TryGetPlatformHandle()!.Handle)throw new InvalidOperationException("回退时测试目标失焦");
                    var before=keyCount;
                    canvas.RaiseEvent(new KeyEventArgs{RoutedEvent=InputElement.KeyDownEvent,Key=Key.F23,Source=canvas});
                    canvas.RaiseEvent(new KeyEventArgs{RoutedEvent=InputElement.KeyUpEvent,Key=Key.F23,Source=canvas});
                    await Wait(()=>keyCount==before+1 && Visible((before&1)==0),"TCP 回退后按键未回显",8000);
                    fallbackVerified=true;Console.WriteLine("媒体组件中断后，原控制会话已通过 TCP 回显按键");
                }
            }
            void Save() {
                var summary=measurements.ToDictionary(m=>m.Key,m=>new {count=m.Value.Count,failed=failures[m.Key],
                    p50_ms=m.Value.Count==0?(double?)null:Percentile(m.Value,.50),p95_ms=m.Value.Count==0?(double?)null:Percentile(m.Value,.95)});
                File.WriteAllText(file,JsonSerializer.Serialize(new {primary.Width,primary.Height,
                    separate_display=secondary!=null,fallback_verified=fallbackVerified,seconds=startCode.Elapsed.TotalSeconds,summary,samples=rows},
                    new JsonSerializerOptions{WriteIndented=true}));
            }
            Save();
        } finally { finish.Cancel(); listener.Join(); ReleaseDC(0,dc);viewer?.Close();target.Close();main.Close(); }
    }
}
