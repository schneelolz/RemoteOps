using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text.Json;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Media.Imaging;
using Avalonia.Platform;
using Avalonia.Threading;

namespace RemoteOps.Desktop;

public partial class ViewerWindow : Window
{
    private readonly DesktopCore core;
    internal string Session {get;}
    private CancellationTokenSource? cancellation;
    private WriteableBitmap? bitmap;
    private bool opened,closed,controlled,pending,hasFrame;
    private static long nextView;
    private long epoch,view;
    private WindowState restoreState = WindowState.Normal;
    private WindowState beforeFullscreen = WindowState.Normal;
    private bool writable, originalPixels;
    private bool? clipboardSupported;
    private readonly string deviceKey;
    private string pictureMode="auto";
    private string transportBackend="auto";
    private int encodedWidth,encodedHeight;
    private readonly DispatcherTimer statsTimer=new(){Interval=TimeSpan.FromSeconds(1)};
    private long windowStart, lastSample;
    private int presented, queuedFrames;
    private double queueTime, decodeTime, receivedRate;
    private int? captureRate;

    private void ToggleStatistics(object? sender, Avalonia.Interactivity.RoutedEventArgs e)
    {
        if(sender is MenuItem item) PerformancePanel.IsVisible=item.IsChecked;
    }

    internal void SetStatistics(JsonElement e)
    {
        lastSample=Stopwatch.GetTimestamp();
        receivedRate=e.GetProperty("receive").GetDouble();
        queuedFrames=e.GetProperty("queued").GetInt32();
        captureRate=e.TryGetProperty("capture",out var c) && c.ValueKind==JsonValueKind.Number && c.TryGetInt32(out var fps) ? fps : null;
    }

    private void RefreshStatistics()
    {
        var seconds=Stopwatch.GetElapsedTime(windowStart).TotalSeconds;
        bool fresh=lastSample!=0 && Stopwatch.GetElapsedTime(lastSample)<TimeSpan.FromSeconds(4);
        string capture=fresh && captureRate.HasValue ? captureRate.Value.ToString() : "—";
        string receive=fresh ? receivedRate.ToString("F1") : "—";
        string waiting=fresh ? queuedFrames.ToString() : "—";
        var display=seconds>0 ? presented/seconds : 0;
        string decoded=presented>0 ? (decodeTime/presented).ToString("F1") : "—";
        string pending=presented>0 ? (queueTime/presented).ToString("F1") : "—";
        PerformanceText.Text=$"采集 {capture}  ·  接收 {receive}  ·  显示 {display:F1} FPS\n解码 {decoded} ms  ·  界面排队 {pending} ms  ·  解码队列 {waiting} 帧";
        windowStart=Stopwatch.GetTimestamp(); presented=0; queueTime=decodeTime=0;
    }
    private readonly DispatcherTimer moveTimer=new(){Interval=TimeSpan.FromMilliseconds(16)};
    private (int x,int y)? pendingMove;
    private readonly HashSet<Key> pressed = [];
    private readonly HashSet<int> buttons = [];

    internal ViewerWindow(DesktopCore core,string session,string name,bool writable,string deviceKey)
    {
        InitializeComponent();
        statsTimer.Tick+=(_,_)=>RefreshStatistics();
        this.core=core;Session=session;this.writable=writable;this.deviceKey=deviceKey;
        ClipboardShare.IsChecked=DesktopPreferences.ClipboardEnabled(deviceKey);
        ClipboardShare.IsCheckedChanged+=(_,_)=>{
            var enabled=ClipboardShare.IsChecked==true;
            try{DesktopPreferences.SaveClipboard(deviceKey,enabled);}
            catch(Exception e) when(e is IOException or UnauthorizedAccessException){Mode.Text="当前选择已生效，偏好写入失败";}
            SyncClipboard();
        };
        UpdateClipboard();
        Toolbar.SizeChanged+=(_,e)=>ArrangeToolbar(e.NewSize.Width);
        moveTimer.Tick+=(_,_)=>FlushMove();
        Title=$"实时桌面 · {name} · v{Program.DisplayVersion}"; NameLabel.Text=name;
        Opened+=(_,_)=>Start();
        Closed+=(_,_)=>{closed=true;Stop(true);};
        Deactivated+=(_,_)=>ReleaseHeld();
        CanvasArea.LostFocus+=(_,_)=>ReleaseHeld();
        CanvasArea.PointerCaptureLost+=(_,_)=>ReleaseHeld();
        Observe.Click+=(_,_)=>ObserveOnly();
        Control.Click+=(_,_)=>{
            if(!writable || pending || controlled || !hasFrame)return;
            if(Post(new {op="control",session=Session,view,epoch})){
                pending=true;Mode.Text="正在切换至控制";UpdateModeButtons();
            }
        };
        Zoom.Click+=(_,_)=>{originalPixels=!originalPixels;SizeCanvas();};
        ScalingChanged+=(_,_)=>SizeCanvas();
        Fullscreen.Click+=(_,_)=>SetFullscreen();
        End.Click+=(_,_)=>Close();
        Retry.Click+=(_,_)=>Start();
        CanvasArea.PointerMoved+=(_,e)=>{if(controlled && Coordinates(e.GetPosition(CanvasArea),buttons.Count>0) is {} p)QueueMove(p);};
        CanvasArea.PointerPressed+=(_,e)=>{
            if(!controlled || Coordinates(e.GetPosition(CanvasArea),false) is not {} p)return;
            var kind=e.GetCurrentPoint(CanvasArea).Properties.PointerUpdateKind;
            int button=kind switch{PointerUpdateKind.LeftButtonPressed=>0,PointerUpdateKind.RightButtonPressed=>1,PointerUpdateKind.MiddleButtonPressed=>2,_=>-1};
            if(button<0)return;
            CanvasArea.Focus();e.Pointer.Capture(CanvasArea);Move(p);
            if(buttons.Add(button))Input(new {Button=new {button,down=true}});
            e.Handled=true;
        };
        CanvasArea.PointerReleased+=(_,e)=>{
            var kind=e.GetCurrentPoint(CanvasArea).Properties.PointerUpdateKind;
            int button=kind switch{PointerUpdateKind.LeftButtonReleased=>0,PointerUpdateKind.RightButtonReleased=>1,PointerUpdateKind.MiddleButtonReleased=>2,_=>-1};
            if(!controlled || !buttons.Remove(button))return;
            if(Coordinates(e.GetPosition(CanvasArea),true) is {} p)Move(p);
            Input(new {Button=new {button,down=false}}); if(buttons.Count==0)e.Pointer.Capture(null);
            e.Handled=true;
        };
        CanvasArea.PointerWheelChanged+=(_,e)=>{
            if(!controlled || Coordinates(e.GetPosition(CanvasArea),false) is not {} p)return;
            Move(p);Input(new {Wheel=new {delta=(short)Math.Clamp(Math.Round(e.Delta.Y*120),short.MinValue,short.MaxValue)}});e.Handled=true;
        };
        // 在画布上先处理远端按键，避免 Tab、空格和回车触发本地工具栏。
        CanvasArea.AddHandler(InputElement.KeyDownEvent, (_, e) => SendKey(e, true), Avalonia.Interactivity.RoutingStrategies.Tunnel);
        CanvasArea.AddHandler(InputElement.KeyUpEvent, (_, e) => SendKey(e, false), Avalonia.Interactivity.RoutingStrategies.Tunnel);
        KeyDown+=(_,e)=>{if(e.Key==Key.F11 && !controlled){SetFullscreen();e.Handled=true;}};
    }

    private void SelectBackend(object? sender, Avalonia.Interactivity.RoutedEventArgs e)
    {
        if(sender is not MenuItem {Tag:string choice} || choice == transportBackend)return;
        transportBackend = choice;
        SetBackendChecks();
        Start();
    }
    private void SetBackendChecks()
    {
        BackendAuto.IsChecked=transportBackend=="auto";
        BackendTcp.IsChecked=transportBackend=="tcp";
        BackendRtc.IsChecked=transportBackend=="minirtc";
        BackendCrossDesk.IsChecked=transportBackend=="crossdesk";
    }

    private void SelectQuality(object? sender, Avalonia.Interactivity.RoutedEventArgs e)
    {
        if(sender is not MenuItem {Tag:string mode} item || PictureQuality.Flyout is not MenuFlyout menu)return;
        if(pictureMode!=mode && opened && !Post(new {op="quality",session=Session,view,quality=mode}))return;
        pictureMode=mode;
        foreach(var entry in menu.Items.OfType<MenuItem>().Where(entry=>entry.Tag is string))entry.IsChecked=entry==item;
        PictureQuality.Content="画质 · "+(mode=="high"?"高清":item.Header);
    }

    private void ArrangeToolbar(double width)
    {
        var actionWidth=ToolbarActions.Children.Where(child=>child.IsVisible).Sum(child=>child.DesiredSize.Width);
        if(actionWidth<=0)return;
        bool narrow=width-actionWidth<315;
        Grid.SetColumnSpan(ToolbarIdentity,narrow?2:1);
        Grid.SetColumn(ToolbarActions,narrow?0:1);
        Grid.SetColumnSpan(ToolbarActions,narrow?2:1);
        Grid.SetRow(ToolbarActions,narrow?1:0);
        ToolbarActions.Margin=new Thickness(0,narrow?6:0,0,0);
    }

    internal void SetClipboardSupport(bool supported)
    {
        if(clipboardSupported==supported)return;
        clipboardSupported=supported;
        SyncClipboard();
    }
    private void UpdateClipboard()
    {
        bool windows=OperatingSystem.IsWindows();
        ClipboardShare.IsEnabled=windows;
        string status=!windows?"此平台不支持剪贴板共享":
            ClipboardShare.IsChecked!=true?"剪贴板共享已关闭":
            clipboardSupported==false?"当前连接未提供剪贴板共享":
            clipboardSupported==null?"正在确认剪贴板能力":
            !controlled || !writable?"剪贴板将在控制模式下共享":"已允许双向复制文本和文件；两端更新后也可复制图片";
        ClipboardState.Text=status;
        ClipboardState.IsVisible=ClipboardShare.IsChecked==true && clipboardSupported==false;
        ToolTip.SetTip(ClipboardShare,status);
        Avalonia.Automation.AutomationProperties.SetHelpText(ClipboardShare,status);
    }
    private void SyncClipboard()
    {
        UpdateClipboard();
        if(clipboardSupported==true && controlled && writable && !pending && epoch>0)
            Post(new {op="clipboard",session=Session,view,epoch,enabled=ClipboardShare.IsChecked==true});
    }

    protected override void OnPropertyChanged(AvaloniaPropertyChangedEventArgs change)
    {
        base.OnPropertyChanged(change);
        if (change.Property == WindowStateProperty && change.GetNewValue<WindowState>() == WindowState.Minimized)
            restoreState = change.GetOldValue<WindowState>();
    }

    internal void RestoreAndActivate()
    {
        if (WindowState == WindowState.Minimized)
        {
            var previous = restoreState;
            // Windows 进入全屏时只调整边框和大小，需先还原最小化的窗口。
            if (previous == WindowState.FullScreen) WindowState = WindowState.Normal;
            WindowState = previous;
        }
        Activate();
    }

    private void SetFullscreen()
    {
        bool enter=WindowState!=WindowState.FullScreen;
        if (enter)
        {
            beforeFullscreen = WindowState;
            if (WindowState == WindowState.Maximized) WindowState = WindowState.Normal;
            WindowState = WindowState.FullScreen;
        }
        else WindowState = beforeFullscreen;
        FullscreenLabel.Text=enter?"退出全屏":"全屏";
        Avalonia.Automation.AutomationProperties.SetName(Fullscreen,FullscreenLabel.Text);
    }
    private bool Post(object cmd)
    {
        try{core.Post(cmd);return true;}
        catch(Exception ex){Stop(true);Failure(ex.Message);return false;}
    }
    private void UpdateModeButtons()
    {
        Control.IsEnabled=writable && hasFrame && epoch>0 && !pending;
        Observe.IsEnabled=hasFrame && !pending;
        Control.Classes.Set("primary",controlled);
        Observe.Classes.Set("primary",!controlled);
        ToolTip.SetTip(Control,writable?"使用本机键鼠操作远程桌面":"当前会话只有观察权限");
    }
    internal void SetWritable(bool allowed)
    {
        writable=allowed;
        if(!allowed && (controlled || pending))ObserveOnly();
        UpdateModeButtons();
    }
    private void ObserveOnly()
    {
        if(!controlled && !pending)return;
        ReleaseHeld();
        controlled=false;
        pending=true;
        UpdateModeButtons();
        Mode.Text="正在切回观察";
        Post(new {op="release",session=Session,view});
    }
    private void Input(object item)
    {
        if(controlled)Post(new {op="input",session=Session,view,epoch,@event=item});
    }
    private void ReleaseHeld()
    {
        CancelMove();
        if(controlled && (pressed.Count!=0 || buttons.Count!=0))Input("Release");
        pressed.Clear();buttons.Clear();
    }
    private void QueueMove((int x,int y) pos)
    {
        pendingMove=pos;
        if(!moveTimer.IsEnabled)moveTimer.Start();
    }
    private void CancelMove()
    {
        moveTimer.Stop();
        pendingMove=null;
    }
    private void FlushMove()
    {
        if(pendingMove is {} pos)Move(pos);
        else moveTimer.Stop();
    }
    private void Move((int x,int y) pos)
    {
        CancelMove();
        Input(new {Move=new {pos.x,pos.y}});
    }
    private (int,int)? Coordinates(Point p,bool clamp)=>Map(p,CanvasArea.Bounds.Size,encodedWidth,encodedHeight,clamp);
    internal static (int,int)? Map(Point p,Size viewport,int w,int h,bool clamp)
    {
        if(w<=0||h<=0||viewport.Width<=0||viewport.Height<=0)return null;
        double scale=Math.Min(viewport.Width/w,viewport.Height/h);
        double width=w*scale,height=h*scale;
        double x=p.X-(viewport.Width-width)/2,y=p.Y-(viewport.Height-height)/2;
        if(!clamp && (x<0||x>=width||y<0||y>=height))return null;
        return ((int)Math.Round(Math.Clamp(x/Math.Max(1,width-1),0,1)*65535),
                (int)Math.Round(Math.Clamp(y/Math.Max(1,height-1),0,1)*65535));
    }
    internal static int VirtualKey(Key key) => key switch
    {
        >=Key.A and <=Key.Z => 0x41+(key-Key.A),
        >=Key.D0 and <=Key.D9 => 0x30+(key-Key.D0),
        >=Key.NumPad0 and <=Key.Divide => 0x60+(key-Key.NumPad0),
        >=Key.F1 and <=Key.F24 => 0x70+(key-Key.F1),
        >=Key.PageUp and <=Key.Down => 0x21+(key-Key.PageUp),
        Key.Back=>0x08, Key.Tab=>0x09, Key.Clear=>0x0C, Key.Enter=>0x0D, Key.Pause=>0x13,
        Key.CapsLock=>0x14, Key.Escape=>0x1B, Key.Space=>0x20, Key.PrintScreen=>0x2C,
        Key.Insert=>0x2D, Key.Delete=>0x2E, Key.LWin=>0x5B, Key.RWin=>0x5C, Key.Apps=>0x5D,
        Key.NumLock=>0x90, Key.Scroll=>0x91,
        Key.LeftShift=>0xA0, Key.RightShift=>0xA1, Key.LeftCtrl=>0xA2, Key.RightCtrl=>0xA3,
        Key.LeftAlt=>0xA4, Key.RightAlt=>0xA5,
        Key.OemSemicolon=>0xBA, Key.OemPlus=>0xBB, Key.OemComma=>0xBC, Key.OemMinus=>0xBD,
        Key.OemPeriod=>0xBE, Key.OemQuestion=>0xBF, Key.OemTilde=>0xC0, Key.OemOpenBrackets=>0xDB,
        Key.OemPipe=>0xDC, Key.OemCloseBrackets=>0xDD, Key.OemQuotes=>0xDE, Key.OemBackslash=>0xE2,
        _=>0
    };

    private void SendKey(KeyEventArgs e,bool down)
    {
        if(!controlled)return;
        int vk=VirtualKey(e.Key);
        if(vk is <=0 or >255)return;
        FlushMove();
        if(down){if(pressed.Add(e.Key))Input(new {Key=new {vk,down}});}
        else if(pressed.Remove(e.Key))Input(new {Key=new {vk,down}});
        e.Handled=true;
    }

    private void Start()
    {
        Stop(true);
        SetBackendChecks();
        Info.Text="正在准备本机观看组件";Mode.Text=Info.Text;Overlay.IsVisible=true;Retry.IsVisible=false;
        var cts=new CancellationTokenSource();
        cancellation=cts;
        lastSample=0; presented=0; queueTime=decodeTime=0; captureRate=null;
        windowStart=Stopwatch.GetTimestamp();
        RefreshStatistics(); statsTimer.Start();
        var token=cts.Token;
        view=Interlocked.Increment(ref nextView);
        long attempt=view;
        var thread=new Thread(()=>Decode(token,attempt)){IsBackground=true,Name="RemoteOps H.264"};
        thread.Start();
    }
    private void Decode(CancellationToken token,long attempt)
    {
        try{
            using var decoder=new DesktopDecoder();
            var packet=new byte[2*1024*1024];var pixels=new byte[1920*1080*4];
            Dispatcher.UIThread.Invoke(()=>{
                if(!token.IsCancellationRequested && !closed)opened=Post(new {op="open",session=Session,view=attempt,quality=pictureMode,backend=transportBackend});
            });
            while(!token.IsCancellationRequested){
                int count=core.ReadFrame(attempt,packet);
                if(count<0)throw new InvalidOperationException("媒体读取失败");
                if(count==0){Thread.Sleep(4);continue;}
                long beforeDecode=Stopwatch.GetTimestamp();
                if(!decoder.Decode(packet,count,pixels,out int w,out int h))continue;
                long ready=Stopwatch.GetTimestamp();
                Dispatcher.UIThread.Invoke(()=>{
                    if(token.IsCancellationRequested || closed || view!=attempt)return;
                    queueTime+=Stopwatch.GetElapsedTime(ready).TotalMilliseconds;
                    decodeTime+=Stopwatch.GetElapsedTime(beforeDecode,ready).TotalMilliseconds;
                    Present(pixels,w,h);
                    presented++;
                });
            }
        }catch(Exception e){
            Dispatcher.UIThread.Post(()=>{if(!token.IsCancellationRequested && !closed){Stop(true);Failure(e.Message);}});
        }
    }
    private void SizeCanvas()
    {
        bool actual=originalPixels && encodedWidth>0 && encodedHeight>0;
        var bars=actual?Avalonia.Controls.Primitives.ScrollBarVisibility.Auto:Avalonia.Controls.Primitives.ScrollBarVisibility.Disabled;
        Viewport.HorizontalScrollBarVisibility=Viewport.VerticalScrollBarVisibility=bars;
        CanvasArea.Width=actual?encodedWidth/RenderScaling:double.NaN;
        CanvasArea.Height=actual?encodedHeight/RenderScaling:double.NaN;
        Zoom.Content=originalPixels?"适应窗口":"100%";
    }

    private void Present(byte[] pixels,int w,int h)
    {
        if(bitmap==null || encodedWidth!=w || encodedHeight!=h){
            var old=bitmap;bitmap=new WriteableBitmap(new PixelSize(w,h),new Vector(96,96),PixelFormat.Bgra8888,AlphaFormat.Opaque);
            Screen.Source=bitmap;old?.Dispose();encodedWidth=w;encodedHeight=h;
            Resolution.Text=$"{w} × {h}";SizeCanvas();
        }
        using(var frame=bitmap.Lock()){
            if(frame.RowBytes==w*4)Marshal.Copy(pixels,0,frame.Address,w*h*4);
            else for(int y=0;y<h;y++)Marshal.Copy(pixels,y*w*4,frame.Address+y*frame.RowBytes,w*4);
        }
        if(!hasFrame){hasFrame=true;Overlay.IsVisible=false;UpdateModeButtons();}
        Screen.InvalidateVisual();
    }
    internal void OpenFailed(JsonElement e,string text)
    {
        if(!Matches(e))return;
        Stop(false);Failure(text);
    }
    internal bool Matches(JsonElement e) => e.TryGetProperty("view",out var id) && id.TryGetInt64(out var value) && value==view;
    internal void SetRoute(JsonElement e,string text)
    {
        if(Matches(e)){CancelMove();Route.Text=text;}
    }
    internal void State(JsonElement e)
    {
        if(!Matches(e))return;
        var state=e.GetProperty("state").GetString();var text=e.GetProperty("text").GetString()??"";
        Mode.Text=text;Info.Text=text;
        if(state is "ended" or "error"){Stop(false);Failure(text);return;}
        var nextEpoch=e.GetProperty("epoch").GetInt64();
        if(epoch!=nextEpoch || state!="control")CancelMove();
        epoch=nextEpoch; controlled=state=="control"; pending=state is "pending" or "releasing";
        if(!controlled){pressed.Clear();buttons.Clear();}
        if(controlled && !writable){ObserveOnly();return;}
        UpdateModeButtons();
        SyncClipboard();
        if(controlled)CanvasArea.Focus();
        if(!hasFrame){Overlay.IsVisible=true;if(state=="view")Info.Text="观察模式 · 正在等待首帧";}
    }
    private void Failure(string text)
    {
        Mode.Text=text;Info.Text=text;Overlay.IsVisible=true;Retry.IsVisible=true;
    }
    private void Stop(bool notify)
    {
        statsTimer.Stop(); PerformanceText.Text="等待实时画面";
        cancellation?.Cancel();cancellation?.Dispose();cancellation=null;
        if(opened && notify){try{core.Post(new {op="end",session=Session,view});}catch(InvalidOperationException){}}
        clipboardSupported=null;UpdateClipboard();
        CancelMove();view=0;
        opened=controlled=pending=hasFrame=false;epoch=0;pressed.Clear();buttons.Clear();UpdateModeButtons();
        encodedWidth=encodedHeight=0;Resolution.Text="";Route.Text="";SizeCanvas();Screen.Source=null;bitmap?.Dispose();bitmap=null;
    }
}
