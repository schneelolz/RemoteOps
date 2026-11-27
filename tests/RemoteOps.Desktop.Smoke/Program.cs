using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text.Json;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Interactivity;
using Avalonia.Input;
using Avalonia.Media.Imaging;
using Avalonia.Styling;
using Avalonia.Threading;
using Avalonia.VisualTree;
using RemoteOps.Desktop;
using SkiaSharp;

internal static class Program
{
    [DllImport("user32.dll")] private static extern nint GetWindow(nint hwnd, uint command);
    [DllImport("user32.dll")] private static extern bool IsIconic(nint hwnd);
    [DllImport("user32.dll")] private static extern bool ShowWindow(nint hwnd, int command);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(nint hwnd);
    [DllImport("window_fixture", CallingConvention = CallingConvention.Cdecl)] private static extern uint window_counts(uint which);

    [DllImport("window_fixture", CallingConvention = CallingConvention.Cdecl)]
    private static extern void window_inject([MarshalAs(UnmanagedType.LPUTF8Str)] string json);

    [STAThread]
    private static int Main(string[] args)
    {
        if (args is ["--relay-latency", var resultFile]) return RelayLatency.Run(resultFile);
        if (args is ["--field-relay-latency", var fieldFile]) return RelayLatency.Run(fieldFile, fieldTarget: true);
        if (args is ["--check-appearance", var savedPath])
        {
            AppContext.SetData("RemoteOps.Desktop.AppearancePath", savedPath);
            AppBuilder.Configure<App>().UsePlatformDetect().SetupWithoutStarting();
            var window = new MainWindow();
            var restored = Application.Current!.RequestedThemeVariant == ThemeVariant.Dark &&
                window.FindControl<RadioButton>("AppearanceDark")!.IsChecked == true;
            window.Close();
            return restored ? 0 : 1;
        }

        var appearancePath = Path.Combine(Path.GetTempPath(), $"RemoteOps.Desktop.Smoke-{Guid.NewGuid():N}.json");
        AppContext.SetData("RemoteOps.Desktop.AppearancePath", appearancePath);
        // 在打开任何产品窗口前将两个产品 ABI 重定向到进程内测试数据。
        var fixture = NativeLibrary.Load(Path.Combine(AppContext.BaseDirectory, "window_fixture.dll"));
        NativeLibrary.SetDllImportResolver(typeof(App).Assembly, (name, _, _) =>
            name is "remoteops_human_controller" or "remoteops_view_decoder" ? fixture : 0);
        AppBuilder.Configure<App>().UsePlatformDetect().SetupWithoutStarting();
        using var done = new CancellationTokenSource();
        int result = 0;
        Dispatcher.UIThread.Post(async () =>
        {
            try { await Check(args, appearancePath); }
            catch (Exception e) { Console.Error.WriteLine(e.Message); result = 1; }
            finally { done.Cancel(); }
        });
        try
        {
            Dispatcher.UIThread.MainLoop(done.Token);
            return result;
        }
        finally
        {
            File.Delete(appearancePath);
        }
    }

    private static async Task Until(Func<bool> condition, string failure)
    {
        var clock = Stopwatch.StartNew();
        while (!condition())
        {
            if (clock.Elapsed > TimeSpan.FromSeconds(5)) throw new InvalidOperationException(failure);
            await Task.Delay(20);
        }
    }

    private static void Require(bool value, string failure)
    {
        if (!value) throw new InvalidOperationException(failure);
    }

    private static async Task Check(string[] args, string appearancePath)
    {
        var main = new MainWindow();
        ViewerWindow? lastViewer = null;
        using var subscription = Window.WindowOpenedEvent.AddClassHandler<ViewerWindow>((w, _) => lastViewer = w);
        main.Show();
        var open = main.FindControl<Button>("OpenDesktop")!;
        void Open() => open.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
        try
        {
            var pair = main.FindControl<Button>("Pair")!;
            var code = main.FindControl<TextBox>("Code")!;
            var service = main.FindControl<TextBlock>("ServiceStatus")!;
            var session = main.FindControl<Border>("DeviceWorkspace")!;
            Require(!pair.IsEnabled && !session.IsVisible, "未选设备时显示了会话工具或允许配对");
            Require(service.Text == "正在连接服务", "启动时服务状态不正确");
            await CheckAppearance(main, appearancePath);
            code.Text = "123-456-789";
            Require(!pair.IsEnabled, "服务连接前允许配对");
            window_inject("""{"kind":"connection","online":false,"retry":true,"text":"身份未配置"}""");
            await Until(() => main.FindControl<Button>("RetryRelay")!.IsVisible, "连接失败没有重试入口");
            Require(!pair.IsEnabled && service.Text == "服务未连接" &&
                main.FindControl<TextBlock>("Status")!.Text == "身份未配置", "连接失败提示或配对状态不正确");
            window_inject("""{"kind":"connection","online":true,"text":"Relay 已连接"}""");
            await Until(() => pair.IsEnabled && open.IsEnabled, "连接就绪后未允许配对");
            Require(service.Text == "服务已连接" && !main.FindControl<TextBlock>("Status")!.IsVisible,
                "连接就绪文案不正确或仍显示连接错误");
            code.Clear();
            await Until(() => !pair.IsEnabled, "未填写控制码时允许配对");
            code.Text = "a2c-d3e-f4g";
            await Until(() => pair.IsEnabled, "输入控制码后未允许配对");
            window_inject("""{"kind":"connection","online":false,"text":"Relay 正在重连"}""");
            await Until(() => !pair.IsEnabled && service.Text == "正在重新连接服务", "服务正在重新连接时状态不正确");
            window_inject("""{"kind":"connection","online":true,"text":"Relay 已连接"}""");
            await Until(() => pair.IsEnabled, "Relay 重连后不能配对");
            // 校验设备能力徽章的四态顺序（离线 / 不支持实时桌面 / 仅观察 / 可控制）。
            Require(MainWindow.DeviceStateBadgeLabel(false, true, true) == "离线", "离线状态徽章错误");
            Require(MainWindow.DeviceStateBadgeLabel(true, false, true) == "在线 · 不支持实时桌面", "不支持实时桌面徽章错误");
            Require(MainWindow.DeviceStateBadgeLabel(true, true, false) == "在线 · 仅观察", "仅观察状态徽章错误");
            Require(MainWindow.DeviceStateBadgeLabel(true, true, true) == "在线 · 可控制", "可控制状态徽章错误");
            var devicesList = main.FindControl<ListBox>("Devices")!;
            var stateText = main.FindControl<TextBlock>("TargetStateText")!;
            Require(stateText.Text == "在线 · 可控制" && open.IsEnabled && session.IsVisible, "默认可控制设备状态不一致");

            Open();
            await Until(() => lastViewer?.FindControl<Image>("Screen")?.Source != null, "未收到测试帧");
            var viewer = lastViewer!;
            foreach (var window in new Window[] {main,viewer})
            {
                var monitor=main.Screens.ScreenFromWindow(main)!;
                var area=monitor.WorkingArea;
                var frame=window.FrameSize!.Value;
                var expectedX=area.X+Math.Max(0,(area.Width-(int)Math.Round(frame.Width*monitor.Scaling))/2);
                var expectedY=area.Y+Math.Max(0,(area.Height-(int)Math.Round(frame.Height*monitor.Scaling))/2);
                Require(Math.Abs(window.Position.X-expectedX)<=2 && Math.Abs(window.Position.Y-expectedY)<=2,
                    $"窗口未在工作区居中：{window.Position}，期望 {expectedX},{expectedY}");
            }
            var actions=viewer.FindControl<WrapPanel>("ToolbarActions")!;
            foreach(var width in new[]{1080,900,660}){
                viewer.Width=width;
                await Task.Delay(100);
                var modePosition=viewer.FindControl<TextBlock>("Mode")!.TranslatePoint(new Point(),viewer);
                var routePosition=viewer.FindControl<TextBlock>("Route")!.TranslatePoint(new Point(),viewer);
                Require(modePosition.HasValue && routePosition.HasValue && Math.Abs(modePosition.Value.X-routePosition.Value.X)<1,"线路换行后左侧缩进");
                if(width<=900)Require(Grid.GetRow(actions)==1,"窄窗口工具未换行");
            }
            viewer.Width=1080;
            var qualityButton=viewer.FindControl<Button>("PictureQuality")!;
            var qualityMenu=(MenuFlyout)qualityButton.Flyout!;
            var transports=qualityMenu.Items.OfType<MenuItem>().Single(i=>Equals(i.Header,"传输方式"))
                .Items.OfType<MenuItem>().ToDictionary(i=>(string)i.Tag!);
            Require(transports.Count==4 && transports["auto"].IsChecked &&
                new[]{"tcp","minirtc","crossdesk"}.All(key=>!transports[key].IsChecked),
                "运维端应默认自动选路，同时保留三个手选后端");
            Require(qualityButton.Content?.ToString()=="画质 · 自动", "默认画质不正确");
            var modes=new[] {"auto","smooth","balanced","high"};
            foreach(var i in new[]{1,2,3,0})
            {
                var item=qualityMenu.Items.OfType<MenuItem>().Single(x=>Equals(x.Tag,modes[i]));
                item.RaiseEvent(new RoutedEventArgs(MenuItem.ClickEvent));
                Require(window_counts(5)==i+1 && item.IsChecked &&
                    qualityMenu.Items.OfType<MenuItem>().Count(x=>x.IsChecked)==1,"画质命令或选中状态错误");
                Require(window_counts(0)==1 && window_counts(1)==0,"切换画质导致桌面重新打开");
            }
            Console.WriteLine("通过：初始窗口居中与观察模式四档画质切换。");
            var statsItem=qualityMenu.Items.OfType<MenuItem>().Single(x=>Equals(x.Header,"性能信息"));
            var statsPanel=viewer.FindControl<Border>("PerformancePanel")!;
            var statsText=viewer.FindControl<TextBlock>("PerformanceText")!;
            Require(!statsPanel.IsVisible,"性能信息应默认折叠");
            statsItem.IsChecked=true;
            statsItem.RaiseEvent(new RoutedEventArgs(MenuItem.ClickEvent));
            Require(statsPanel.IsVisible,"未展开性能信息");
            window_inject("""{"kind":"video_stats","session":"test","view":1,"receive":22.0,"capture":28,"queued":2}""");
            await Until(()=>statsText.Text?.Contains("采集 28")==true && statsText.Text.Contains("解码队列 2 帧"),"采集或接收统计未展示");
            window_inject("""{"kind":"video_stats","session":"test","view":1,"receive":20.0,"capture":null,"queued":0}""");
            await Until(()=>statsText.Text?.Contains("采集 —")==true && statsText.Text.Contains("接收 20"),"旧 Agent 的缺失采集统计应显示未知");
            statsItem.IsChecked=false;
            statsItem.RaiseEvent(new RoutedEventArgs(MenuItem.ClickEvent));
            Require(!statsPanel.IsVisible && window_counts(0)==1,"折叠性能信息不应重开画面");
            Console.WriteLine("通过：性能信息开关、采集和接收帧率、缺失采样兼容。");
            var clip = viewer.FindControl<ToggleSwitch>("ClipboardShare")!;
            Require(clip.IsEnabled, "等待能力协商时不能设置剪贴板偏好");
            var originalWidth = viewer.Width;
            foreach (var width in new[] { 1080d, 700d })
            {
                viewer.Width=width;
                await Task.Delay(100);
                var center=clip.TranslatePoint(new Point(clip.Bounds.Width/2,clip.Bounds.Height/2),viewer);
                var hit=center.HasValue ? viewer.InputHitTest(center.Value) as Visual : null;
                Require(hit==clip || hit?.GetVisualAncestors().Contains(clip)==true, $"剪贴板点击被遮挡，窗口宽 {width}");
            }
            viewer.Width=originalWidth;
            var choice = clip.IsChecked;
            try
            {
                clip.IsChecked=false;
                clip.Focus();
                clip.RaiseEvent(new KeyEventArgs { RoutedEvent=InputElement.KeyDownEvent, Key=Key.Space });
                clip.RaiseEvent(new KeyEventArgs { RoutedEvent=InputElement.KeyUpEvent, Key=Key.Space });
                Require(clip.IsChecked==true, "未协商能力时空格不能切换剪贴板");
                window_inject("""{"kind":"clipboard_support","session":"test","view":1,"available":false}""");
                await Until(() => viewer.FindControl<TextBlock>("ClipboardState")!.IsVisible, "缺少能力时未提示");
                Require(clip.IsEnabled, "旧端点无法预设剪贴板偏好");
                // 第一扇观看窗口的编号是 1。先取得控制，再迟到达能力消息。
                window_inject("""{"kind":"desktop","session":"test","view":1,"state":"control","epoch":1,"text":"控制模式"}""");
                await Until(() => viewer.FindControl<TextBlock>("Mode")!.Text == "控制模式", "未切换至控制模式");
                var canvas = viewer.FindControl<Grid>("CanvasArea")!;
                var beforeKeys = window_counts(6);
                var beforeReleases = window_counts(7);
                foreach (var key in new[] { Key.A, Key.D1, Key.Space, Key.Tab, Key.Enter, Key.LeftCtrl })
                {
                    canvas.RaiseEvent(new KeyEventArgs { RoutedEvent = InputElement.KeyDownEvent, Key = key });
                    canvas.RaiseEvent(new KeyEventArgs { RoutedEvent = InputElement.KeyUpEvent, Key = key });
                }
                Require(window_counts(6) == beforeKeys + 12 && window_counts(7) == beforeReleases &&
                    viewer.FindControl<Button>("Control")!.Classes.Contains("primary"), "连续输入后应保持控制并转发全部键盘事件");
                var route = viewer.FindControl<TextBlock>("Route")!;
                var log = main.FindControl<TextBox>("Log")!;
                var previousRoute = route.Text;
                window_inject("""{"kind":"route_detail","session":"test","view":99,"text":"过期连接诊断"}""");
                window_inject("""{"kind":"route_detail","session":"test","view":1,"text":"直连 #1234abcd · 发起 TCP 连接"}""");
                await Until(() => log.Text?.Contains("#1234abcd") == true, "当前直连诊断未进入操作记录");
                Require(log.Text?.Contains("过期连接诊断") == false && route.Text == previousRoute, "过程日志串到其他窗口或改变路由状态");
                window_inject("""{"kind":"route","session":"test","view":1,"text":"中继连接 · 直连 #1234abcd · TCP 连接超时"}""");
                await Until(() => route.Text?.Contains("TCP 连接超时") == true, "未显示直连失败阶段");
                Require(viewer.FindControl<TextBlock>("Mode")!.Text == "控制模式" &&
                    viewer.FindControl<Image>("Screen")!.Source != null, "直连失败后丢失控制模式或中继画面");
                Console.WriteLine("通过：当前开窗的直连诊断、TCP 失败提示及中继画面状态。");
                Require(window_counts(3)==0 && window_counts(4)==0, "未协商能力时发送了剪贴板请求");
                window_inject("""{"kind":"clipboard_support","session":"test","view":1,"available":true}""");
                await Until(() => window_counts(3)==1, "收到能力后未应用已开启的偏好");
                clip.IsChecked = false;
                await Until(() => window_counts(4)==1, "关闭开关没有停止共享");
                clip.IsChecked = true;
                await Until(() => window_counts(3)==2, "重新打开没有启动共享");
                window_inject("""{"kind":"desktop","session":"test","view":1,"state":"view","epoch":2,"text":"观察模式"}""");
                await Until(() => viewer.FindControl<TextBlock>("Mode")!.Text=="观察模式", "未切回观察");
                clip.IsChecked = false; clip.IsChecked = true;
                Require(window_counts(3)==2 && window_counts(4)==1, "观察模式发送了剪贴板请求");
            }
            finally { clip.IsChecked = choice; }
            Console.WriteLine("通过：配对就绪门控、剪贴板开关、迟到能力和观察模式。");
            if (args.Length > 0)
            {
                Directory.CreateDirectory(args[0]);
                var failures = new List<string>();
                foreach (var theme in new[] { ThemeVariant.Light, ThemeVariant.Dark })
                {
                    Application.Current!.RequestedThemeVariant = theme;
                    await Task.Delay(100);
                    foreach (var scale in new[] { 1.0, 1.25, 1.5 })
                    {
                        Snapshot(main, scale, args[0], theme.Key + "-main", failures);
                        Snapshot(viewer, scale, args[0], theme.Key + "-viewer", failures);
                    }
                    // 切换到在线可写但不支持实时桌面的设备，校验徽章与按钮禁用一致并截图。
                    devicesList.SelectedIndex = 1;
                    await Task.Delay(60);
                    Require(stateText.Text == "在线 · 不支持实时桌面" && !open.IsEnabled,
                        $"不支持实时桌面设备状态不一致：{stateText.Text}, OpenDesktop.IsEnabled={open.IsEnabled}");
                    RenderWindowImage(main, 1.0, args[0], theme.Key + "-main-no-desktop");
                    devicesList.SelectedIndex = 0;
                    await Task.Delay(60);
                    Require(stateText.Text == "在线 · 可控制" && open.IsEnabled, "切回默认设备后状态未恢复");
                }
                window_inject("""{"kind":"devices","items":[]}""");
                await Until(() => main.FindControl<StackPanel>("EmptyDevicesHint")!.IsVisible && !session.IsVisible,
                    "清空设备列表后未恢复未配对界面");
                code.Clear();
                foreach (var theme in new[] { ThemeVariant.Light, ThemeVariant.Dark })
                {
                    Application.Current!.RequestedThemeVariant = theme;
                    await Task.Delay(100);
                    RenderWindowImage(main, 1.0, args[0], theme.Key + "-main-empty");
                }
                var originalSize = main.Bounds.Size;
                main.Width = main.MinWidth;
                main.Height = main.MinHeight;
                await Until(() => main.Bounds.Width <= main.MinWidth + 1 && main.Bounds.Height <= main.MinHeight + 1,
                    "未缩小到主窗口最小尺寸");
                foreach (var name in new[] { "AppearanceAuto", "AppearanceLight", "AppearanceDark", "Code", "Pair", "LogPanel" })
                {
                    var control = main.FindControl<Control>(name)!;
                    var origin = control.TranslatePoint(default, main);
                    Require(origin.HasValue && origin.Value.X >= 0 && origin.Value.Y >= 0 &&
                        origin.Value.X + control.Bounds.Width <= main.Bounds.Width &&
                        origin.Value.Y + control.Bounds.Height <= main.Bounds.Height, $"最小窗口裁切控件 {name}");
                }
                RenderWindowImage(main, 1.0, args[0], "Dark-main-empty-min");
                main.Width = originalSize.Width;
                main.Height = originalSize.Height;
                window_inject("""{"kind":"devices","items":[{"session":"test","device_id":"unit","name":"连接 1","online":true,"desktop":true,"writable":true}]}""");
                await Until(() => open.IsEnabled, "恢复测试设备后未恢复会话操作");
                if (failures.Count > 0) throw new InvalidOperationException(string.Join(Environment.NewLine, failures));
            }
            else
            {
                devicesList.SelectedIndex = 1;
                await Task.Delay(40);
                Require(stateText.Text == "在线 · 不支持实时桌面" && !open.IsEnabled,
                    $"不支持实时桌面设备状态不一致：{stateText.Text}, OpenDesktop.IsEnabled={open.IsEnabled}");
                devicesList.SelectedIndex = 0;
                await Task.Delay(40);
            }
            var mainHandle = main.TryGetPlatformHandle()!.Handle;
            var viewHandle = viewer.TryGetPlatformHandle()!.Handle;
            var source = viewer.FindControl<Image>("Screen")!.Source;
            Require(GetWindow(viewHandle, 4 /* GW_OWNER */) == 0, "Windows 仍将主窗口作为实时桌面的 owner");
            ShowWindow(mainHandle, 6 /* SW_MINIMIZE */);
            await Until(() => IsIconic(mainHandle), "主窗口没有最小化");
            Require(IsWindowVisible(viewHandle) && !IsIconic(viewHandle), "最小化主窗口连带隐藏了实时桌面");
            ShowWindow(mainHandle, 9 /* SW_RESTORE */);
            await Until(() => !IsIconic(mainHandle), "主窗口没有还原");
            foreach (var state in new[] { WindowState.Normal, WindowState.Maximized, WindowState.FullScreen })
            {
                if (state == WindowState.FullScreen)
                    viewer.FindControl<Button>("Fullscreen")!.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                else viewer.WindowState = state;
                await Until(() => viewer.WindowState == state, "进入窗口状态失败 " + state);
                await Task.Delay(100);
                // 模拟任务栏最小化；全屏还覆盖 Avalonia 主动改变窗口状态的路径。
                if (state == WindowState.FullScreen) viewer.WindowState = WindowState.Minimized;
                else ShowWindow(viewHandle, 6 /* SW_MINIMIZE */);
                await Until(() => IsIconic(viewHandle), "实时桌面没有最小化");
                Require(IsWindowVisible(mainHandle) && !IsIconic(mainHandle), "最小化实时桌面连带隐藏了主窗口");
                Open();
                await Until(() => !IsIconic(viewHandle) && viewer.WindowState == state, "实时桌面未还原至 " + state);
                Require(lastViewer == viewer && viewer.FindControl<Image>("Screen")!.Source == source &&
                    window_counts(0) == 1 && window_counts(1) == 0, "恢复窗口重开了会话或丢失了画面");
            }
            ShowWindow(viewHandle, 6 /* SW_MINIMIZE */);
            await Until(() => IsIconic(viewHandle), "任务栏未最小化全屏桌面");
            Open();
            await Until(() => !IsIconic(viewHandle) && viewer.WindowState == WindowState.FullScreen, "任务栏最小化后未恢复全屏");
            Require(window_counts(0) == 1 && window_counts(1) == 0, "任务栏还原重开了共享");
            viewer.FindControl<Button>("Fullscreen")!.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            await Until(() => viewer.WindowState == WindowState.Maximized, "退出全屏未回到原窗口状态");
            viewer.Close();
            await Until(() => window_counts(1) == 1, "关闭实时桌面未结束共享");
            Require(window_counts(2) == 0 && main.IsVisible && open.IsEnabled, "关闭实时桌面导致整个连接关闭");
            Open();
            await Until(() => lastViewer != viewer && lastViewer!.FindControl<Image>("Screen")!.Source != null, "无法再次打开");
            window_inject("""{"kind":"paired","pair_code":"123-456-789","device":{"session":"fixture-pair","device_id":"pair-host","hostname":"FIELD-PC","name":"连接 99","online":true,"desktop":false,"writable":false}}""");
            var target=main.FindControl<TextBlock>("Target")!;
            var copy=main.FindControl<Button>("CopyPairCode")!;
            await Until(()=>target.Text=="123-456-789" && copy.IsVisible,"已配对设备没有显示本次配对码");
            window_inject("""{"kind":"devices","items":[{"session":"fixture-pair","device_id":"pair-host","hostname":"FIELD-PC","name":"连接 99","online":true,"desktop":false,"writable":false}]}""");
            await Task.Delay(120);
            Require(target.Text=="123-456-789" && main.FindControl<TextBlock>("DeviceCaption")!.Text!.Contains("FIELD-PC"),"刷新后配对码或主机名丢失");
            window_inject("""{"kind":"devices","items":[{"session":"fixture-pair","device_id":"other-host","hostname":"ANOTHER-PC","name":"连接 99","online":true,"desktop":false,"writable":false}]}""");
            await Until(()=>target.Text=="ANOTHER-PC" && !copy.IsVisible,"新设备误用旧设备配对码");
            main.Close();
            Require(!lastViewer!.IsVisible && window_counts(1) == 2 && window_counts(2) == 1, "退出控制端未清理实时桌面和连接");
            Console.WriteLine("通过：独立最小化、还原普通/最大化/全屏、保留同一画面和会话、单独关闭与整体退出。");
        }
        finally { if (main.IsVisible) main.Close(); }
    }

    private static async Task CheckAppearance(MainWindow main, string appearancePath)
    {
        var application = Application.Current!;
        var auto = main.FindControl<RadioButton>("AppearanceAuto")!;
        var light = main.FindControl<RadioButton>("AppearanceLight")!;
        var dark = main.FindControl<RadioButton>("AppearanceDark")!;
        void Select(RadioButton button) => button.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
        string? SavedTheme()
        {
            using var document = JsonDocument.Parse(File.ReadAllText(appearancePath));
            return document.RootElement.GetProperty("Theme").GetString();
        }

        Require(auto.IsChecked == true && application.RequestedThemeVariant == ThemeVariant.Default, "首次启动未默认跟随系统");
        Select(light);
        Require(light.IsChecked == true && application.RequestedThemeVariant == ThemeVariant.Light && SavedTheme() == "light", "浅色选项未切换或未保存");
        Select(dark);
        Require(dark.IsChecked == true && light.IsChecked != true &&
            application.RequestedThemeVariant == ThemeVariant.Dark && SavedTheme() == "dark", "深色选项未切换或未保存");
        var runner = new ProcessStartInfo(Environment.ProcessPath!) { UseShellExecute = false, CreateNoWindow = true };
        if (Path.GetFileNameWithoutExtension(runner.FileName).Equals("dotnet", StringComparison.OrdinalIgnoreCase))
            runner.ArgumentList.Add(typeof(Program).Assembly.Location);
        runner.ArgumentList.Add("--check-appearance");
        runner.ArgumentList.Add(appearancePath);
        using var reopened = Process.Start(runner)!;
        await reopened.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(12));
        Require(reopened.ExitCode == 0, "重启后未恢复深色选项");
        Select(auto);
        Require(auto.IsChecked == true && dark.IsChecked != true &&
            application.RequestedThemeVariant == ThemeVariant.Default && SavedTheme() == "auto", "切回自动后未恢复跟随系统或未保存");
        Console.WriteLine("通过：默认跟随系统、手动切换浅色和深色、重启恢复以及切回自动。");
    }

    private static string RenderWindowImage(Window window, double scale, string folder, string name)
    {
        var content = (Control)window.Content!;
        // RenderTargetBitmap 只渲染窗口内容，补上原生窗口提供的底色。
        if (content is Panel panel) panel.Background = window.Background;
        var size = content.Bounds.Size;
        var pixels = PixelSize.FromSize(size, scale);
        using var target = new RenderTargetBitmap(pixels, new Vector(96 * scale, 96 * scale));
        target.Render(content);
        var file = Path.Combine(folder, $"{name}-{scale * 100:F0}.png");
        target.Save(file);
        return file;
    }

    private static void Snapshot(Window window, double scale, string folder, string name, List<string> failures)
    {
        var content = (Control)window.Content!;
        var file = RenderWindowImage(window, scale, folder, name);
        using var image = SKBitmap.Decode(file);
        var rows = new List<(string, Control)>();
        if (window is MainWindow)
        {
            foreach (var id in new[] { "SettingsButton", "Pair", "OpenDesktop", "ToolsButton", "Disconnect", "Refresh", "Code", "LogPanel",
                         "AppearanceAuto", "AppearanceLight", "AppearanceDark" })
                if (window.FindControl<Control>(id) is { IsVisible: true } control) rows.Add((id, control));
            var devices = window.FindControl<ListBox>("Devices")!;
            rows.Add(("ListBoxItem", devices.ContainerFromIndex(0)!));
        }
        else
        {
            rows.Add(("Zoom", window.FindControl<Button>("Zoom")!));
        }
        if (window is ViewerWindow)
        {
            rows.Add(("Appearance", content.GetVisualDescendants().OfType<AppearanceButton>().Single()));
            rows.Add(("Fullscreen", window.FindControl<Button>("Fullscreen")!));
            rows.Add(("End", window.FindControl<Button>("End")!));
        }
        foreach (var (id, control) in rows)
        {
            var origin = control.TranslatePoint(default, content)!.Value;
            int X(double dip) => (int)Math.Round(dip * scale);
            int left = X(origin.X), top = X(origin.Y);
            int right = X(origin.X + control.Bounds.Width), bottom = X(origin.Y + control.Bounds.Height);
            var background = image.GetPixel(left + X(4), (top + bottom) / 2);
            int textLeft = left + X(8), textRight = right - X(8);
            if (id is "Fullscreen" or "End")
            {
                var label = control.GetVisualDescendants().OfType<TextBlock>().First();
                var labelOrigin = label.TranslatePoint(default, content)!.Value;
                textLeft = X(labelOrigin.X); textRight = X(labelOrigin.X + label.Bounds.Width);
            }
            if (id == "LogPanel") textRight = left + X(80);
            var ink = new List<int>();
            for (int y = top + X(4); y < bottom - X(4); y++)
            {
                int marked = 0;
                for (int x = textLeft; x < textRight; x++)
                {
                    var c = image.GetPixel(x, y);
                    if (Math.Max(Math.Abs(c.Red-background.Red), Math.Max(Math.Abs(c.Green-background.Green),
                        Math.Abs(c.Blue-background.Blue))) >= 65) marked++;
                }
                if (marked >= 2) ink.Add(y);
            }
            if (ink.Count == 0) throw new InvalidOperationException("无法测量文字 " + id);
            int above = ink[0] - top, below = bottom - 1 - ink[^1];
            Console.WriteLine($"{name} {scale:P0} {id} 上{above} 下{below}");
            // 留出一个物理像素抗锯齿误差，以及字形之间的自然差异。
            if (Math.Abs(above - below) > Math.Ceiling(2 * scale) + 1)
                failures.Add($"{name} {scale:P0} {id} 文字未居中，上{above} 下{below}");
        }
    }
}
