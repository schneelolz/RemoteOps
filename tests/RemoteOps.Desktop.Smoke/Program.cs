using System.Diagnostics;
using System.Runtime.InteropServices;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Interactivity;
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

    [STAThread]
    private static int Main(string[] args)
    {
        // 在打开任何产品窗口前将两个产品 ABI 重定向到进程内测试数据。
        var fixture = NativeLibrary.Load(Path.Combine(AppContext.BaseDirectory, "window_fixture.dll"));
        NativeLibrary.SetDllImportResolver(typeof(App).Assembly, (name, _, _) =>
            name is "remoteops_human_controller" or "remoteops_view_decoder" ? fixture : 0);
        AppBuilder.Configure<App>().UsePlatformDetect().SetupWithoutStarting();
        using var done = new CancellationTokenSource();
        int result = 0;
        Dispatcher.UIThread.Post(async () =>
        {
            try { await Check(args); }
            catch (Exception e) { Console.Error.WriteLine(e.Message); result = 1; }
            finally { done.Cancel(); }
        });
        Dispatcher.UIThread.MainLoop(done.Token);
        return result;
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

    private static async Task Check(string[] args)
    {
        var main = new MainWindow();
        ViewerWindow? lastViewer = null;
        using var subscription = Window.WindowOpenedEvent.AddClassHandler<ViewerWindow>((w, _) => lastViewer = w);
        main.Show();
        var open = main.FindControl<Button>("OpenDesktop")!;
        void Open() => open.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
        try
        {
            await Until(() => open.IsEnabled, "虚拟设备未显示");
            Open();
            await Until(() => lastViewer?.FindControl<Image>("Screen")?.Source != null, "未收到测试帧");
            var viewer = lastViewer!;
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
                }
                if (failures.Count > 0) throw new InvalidOperationException(string.Join(Environment.NewLine, failures));
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
            main.Close();
            Require(!lastViewer!.IsVisible && window_counts(1) == 2 && window_counts(2) == 1, "退出控制端未清理实时桌面和连接");
            Console.WriteLine("通过：独立最小化、还原普通/最大化/全屏、保留同一画面和会话、单独关闭与整体退出。");
        }
        finally { if (main.IsVisible) main.Close(); }
    }

    private static void Snapshot(Window window, double scale, string folder, string name, List<string> failures)
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
        using var image = SKBitmap.Decode(file);
        var rows = new List<(string, Control)>();
        if (window is MainWindow)
        {
            foreach (var id in new[] { "SettingsButton", "Pair", "OpenDesktop", "ToolsButton", "Disconnect", "Refresh", "Code", "LogPanel" })
                rows.Add((id, window.FindControl<Control>(id)!));
            var devices = window.FindControl<ListBox>("Devices")!;
            rows.Add(("ListBoxItem", devices.ContainerFromIndex(0)!));
        }
        else
        {
            rows.Add(("Zoom", window.FindControl<Button>("Zoom")!));
        }
        rows.Add(("Appearance", content.GetVisualDescendants().OfType<AppearanceButton>().Single()));
        if (window is ViewerWindow)
        {
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
