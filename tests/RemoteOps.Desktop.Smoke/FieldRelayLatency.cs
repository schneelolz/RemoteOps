using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text.Json;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;
using Avalonia.Media.Imaging;
using RemoteOps.Desktop;

/// <summary>通过双机真实画面测量按键回显，所有计时均使用控制端的单调时钟。</summary>
internal static class FieldRelayLatency
{
    [DllImport("user32")] private static extern nint WindowFromPoint(PixelPoint point);
    [DllImport("user32")] private static extern nint GetAncestor(nint window, uint flag);
    [DllImport("user32")] private static extern nint GetDC(nint window);
    [DllImport("user32")] private static extern int ReleaseDC(nint window, nint dc);
    [DllImport("gdi32")] private static extern uint GetPixel(nint dc, int x, int y);

    private static int ReadSequence(nint dc, nint owner, Image image)
    {
        if (image.Source is not Bitmap frame) return -1;
        var size = frame.Size;
        var bounds = image.Bounds;
        if (size.Width <= 0 || size.Height <= 0 || bounds.Width <= 0 || bounds.Height <= 0) return -1;
        var scale = Math.Min(bounds.Width / size.Width, bounds.Height / size.Height);
        var displayedWidth = scale * size.Width;
        int sequence = 0;
        for (var tile = 0; tile < 5; tile++)
        {
            var x = (bounds.Width - displayedWidth) / 2 + displayedWidth * (tile + 3) / 10;
            var point = image.PointToScreen(new Point(x, bounds.Height / 2));
            if (GetAncestor(WindowFromPoint(point), 2) != owner) return -1;
            var pixel = GetPixel(dc, point.X, point.Y);
            if (pixel == uint.MaxValue) return -1;
            int r = (byte)pixel, g = (byte)(pixel >> 8), b = (byte)(pixel >> 16);
            if (tile == 0) { if (r > 90 || g < 165 || b < 165) return -1; }
            else if (tile == 4) { if (r < 165 || g < 165 || b > 90) return -1; }
            else if (r < 90 && g > 165 && b < 90) sequence |= 1 << (tile - 1);
            else if (r < 165 || g > 90 || b > 90) return -1;
        }
        return sequence;
    }

    private sealed record Sample(string Transport, int Block, int Index, double Milliseconds);

    internal static async Task Measure(string code, string file)
    {
        ViewerWindow? viewer = null;
        using var hook = Window.WindowOpenedEvent.AddClassHandler<ViewerWindow>((window, _) => viewer = window);
        var main = new MainWindow();
        var dc = GetDC(0);
        var samples = new List<Sample>();
        string[] order = ["tcp", "minirtc", "crossdesk", "crossdesk", "minirtc", "tcp"];
        int block = 0, index = 0, timeouts = 0;
        PixelSize? encoded = null;

        void Save(string status)
        {
            static double Quantile(double[] sorted, double p) => sorted[(int)Math.Ceiling(sorted.Length * p) - 1];
            var summary = samples.GroupBy(s => s.Transport).ToDictionary(group => group.Key, group =>
            {
                var times = group.Select(s => s.Milliseconds).Order().ToArray();
                return new { count = times.Length, p50_ms = Quantile(times, .50), p95_ms = Quantile(times, .95) };
            });
            var report = new { setup = "two_host", status, encoded = encoded?.ToString(), order, block,
                index, timeouts, summary, samples };
            File.WriteAllText(file + ".tmp", JsonSerializer.Serialize(report, new JsonSerializerOptions { WriteIndented = true }));
            File.Move(file + ".tmp", file, true);
        }

        try
        {
            if (dc == 0) throw new InvalidOperationException("无法读取控制端桌面");
            main.Show();
            main.FindControl<TextBox>("Code")!.Text = code;
            var pair = main.FindControl<Button>("Pair")!;
            await RelayLatency.Wait(() => pair.IsEnabled, "等待控制端连接 Relay");
            pair.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            await RelayLatency.Wait(() => viewer?.FindControl<Image>("Screen")?.Source != null, "等待配对和首帧", 35000);
            var window = viewer!;
            var hwnd = window.TryGetPlatformHandle()!.Handle;
            var image = window.FindControl<Image>("Screen")!;
            var mode = window.FindControl<TextBlock>("Mode")!;
            var route = window.FindControl<TextBlock>("Route")!;
            var control = window.FindControl<Button>("Control")!;
            var canvas = window.FindControl<Grid>("CanvasArea")!;
            window.FindControl<ToggleSwitch>("ClipboardShare")!.IsChecked = false;
            var menu = (MenuFlyout)window.FindControl<Button>("PictureQuality")!.Flyout!;
            menu.Items.OfType<MenuItem>().Single(i => Equals(i.Tag, "balanced")).RaiseEvent(new RoutedEventArgs(MenuItem.ClickEvent));
            var submenu = menu.Items.OfType<MenuItem>().Single(i => Equals(i.Header, "传输方式"));

            foreach (var transport in order)
            {
                block++;
                index = 0;
                var expected = transport switch { "tcp" => "TCP 中继", "minirtc" => "MiniRTC · TURN/UDP 中继", _ => "CrossDesk · TURN 中继" };
                submenu.Items.OfType<MenuItem>().Single(i => Equals(i.Tag, transport)).RaiseEvent(new RoutedEventArgs(MenuItem.ClickEvent));
                await RelayLatency.Wait(() => route.Text == expected && control.IsEnabled, "等待 " + expected, 30000);
                if (mode.Text != "控制模式") control.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                await RelayLatency.Wait(() => mode.Text == "控制模式" && ReadSequence(dc, hwnd, image) >= 0,
                    "等待控制权限和前台回显窗口", 35000);
                await Task.Delay(500);
                bool Ready() => route.Text == expected && mode.Text == "控制模式" && image.Source is Bitmap;
                for (index = 0; index < 25; index++)
                {
                    if (!Ready()) throw new InvalidOperationException("传输方式或控制模式发生变化");
                    var frameSize = ((Bitmap)image.Source!).PixelSize;
                    encoded ??= frameSize;
                    if (encoded != frameSize) throw new InvalidOperationException("三路编码分辨率不一致");
                    int before = ReadSequence(dc, hwnd, image);
                    if (before < 0) throw new InvalidOperationException("现场回显窗口失焦或控制端采样点被遮挡");
                    var start = Stopwatch.GetTimestamp();
                    canvas.RaiseEvent(new KeyEventArgs { RoutedEvent = InputElement.KeyDownEvent, Key = Key.F23, Source = canvas });
                    canvas.RaiseEvent(new KeyEventArgs { RoutedEvent = InputElement.KeyUpEvent, Key = Key.F23, Source = canvas });
                    await RelayLatency.Wait(() =>
                    {
                        if (!Ready() || ((Bitmap)image.Source!).PixelSize != encoded)
                            throw new InvalidOperationException("采样期间路由、权限或分辨率发生变化");
                        return ReadSequence(dc, hwnd, image) == (before + 1) % 8;
                    }, "等待现场序号变化", 6000);
                    var elapsed = Stopwatch.GetElapsedTime(start).TotalMilliseconds;
                    if (index != 0) samples.Add(new Sample(transport, block, index, Math.Round(elapsed, 1)));
                    await Task.Delay(150 + index * 13 % 90);
                }
                Save("running");
            }
            Save("complete");
        }
        catch (TimeoutException error) { timeouts++; Save(error.Message); throw; }
        catch (Exception error) { Save(error.GetType().Name); throw; }
        finally { if (dc != 0) ReleaseDC(0, dc); viewer?.Close(); main.Close(); }
    }
}
