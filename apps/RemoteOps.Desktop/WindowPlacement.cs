using System.Runtime.InteropServices;
using Avalonia;
using Avalonia.Controls;

namespace RemoteOps.Desktop;

internal static class WindowPlacement
{
    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out System.Drawing.Point point);

    // 启动时根据鼠标或发起窗口选择显示器，不建立窗口所有权关系。
    internal static void CenterOnOpen(Window window, Window? source = null)
    {
        var screen = source == null ? null : source.Screens.ScreenFromWindow(source);
        if (screen == null && OperatingSystem.IsWindows() && GetCursorPos(out var cursor))
            screen = window.Screens.ScreenFromPoint(new PixelPoint(cursor.X, cursor.Y));
        screen ??= window.Screens.Primary;
        if (screen == null) return;

        var area = screen.WorkingArea;
        PixelPoint Center(Size size) => new(
            area.X + Math.Max(0, (area.Width - (int)Math.Round(size.Width * screen.Scaling)) / 2),
            area.Y + Math.Max(0, (area.Height - (int)Math.Round(size.Height * screen.Scaling)) / 2));

        window.WindowStartupLocation = WindowStartupLocation.Manual;
        window.Position = Center(new Size(window.Width, window.Height));
        window.Opened += FirstOpen;
        void FirstOpen(object? sender, EventArgs args)
        {
            window.Opened -= FirstOpen;
            if (window.WindowState == WindowState.Normal && window.FrameSize is { } frame)
                window.Position = Center(frame);
        }
    }
}
