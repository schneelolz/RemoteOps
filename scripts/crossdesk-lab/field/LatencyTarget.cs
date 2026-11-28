using System;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Windows.Forms;

// 双机验收时在现场交互桌面运行。测试只改变本窗体画出的序号。
internal sealed class LatencyTarget : Form
{
    private const int HotkeyId = 23;
    private int sequence;
    private bool registered;

    [DllImport("user32.dll")] private static extern bool RegisterHotKey(IntPtr window, int id, uint modifiers, uint key);
    [DllImport("user32.dll")] private static extern bool UnregisterHotKey(IntPtr window, int id);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern bool SetProcessDPIAware();

    private LatencyTarget()
    {
        Text = "RemoteOps 按键回显测试";
        DoubleBuffered = true;
        BackColor = Color.FromArgb(30, 36, 42);
        ForeColor = Color.White;
        FormBorderStyle = FormBorderStyle.None;
        StartPosition = FormStartPosition.Manual;
        Bounds = Screen.PrimaryScreen.Bounds;
    }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        registered = RegisterHotKey(Handle, HotkeyId, 0x4000, (uint)Keys.F23);
        if (!registered)
        {
            MessageBox.Show(this, "测试键 F23 已被占用，请关闭另一份计时窗口后重试。", Text);
            Close();
            return;
        }
        Invalidate();
    }

    protected override void WndProc(ref Message message)
    {
        if (message.Msg == 0x0312 && message.WParam.ToInt32() == HotkeyId)
        {
            if (GetForegroundWindow() == Handle)
            {
                sequence = (sequence + 1) % 8;
                Invalidate();
            }
            return;
        }
        base.WndProc(ref message);
    }

    protected override void OnActivated(EventArgs e) { base.OnActivated(e); Invalidate(); }
    protected override void OnDeactivate(EventArgs e) { base.OnDeactivate(e); Invalidate(); }

    protected override void OnKeyDown(KeyEventArgs e)
    {
        if (e.KeyCode == Keys.Escape) Close();
        else base.OnKeyDown(e);
    }

    protected override void OnPaint(PaintEventArgs e)
    {
        base.OnPaint(e);
        int width = ClientSize.Width, height = ClientSize.Height;
        TextRenderer.DrawText(e.Graphics, "RemoteOps 按键回显测试  ·  按 Esc 退出", SystemFonts.CaptionFont,
            new Rectangle(0, height / 4, width, height / 8), ForeColor, TextFormatFlags.HorizontalCenter | TextFormatFlags.VerticalCenter);
        if (!registered || GetForegroundWindow() != Handle) return;
        for (int tile = 0; tile < 5; tile++)
        {
            Brush brush = Brushes.Cyan;
            if (tile == 4) brush = Brushes.Yellow;
            else if (tile > 0) brush = (sequence & (1 << (tile - 1))) == 0 ? Brushes.Red : Brushes.Lime;
            int center = width * (3 + tile) / 10;
            e.Graphics.FillRectangle(brush, center - width / 32, height / 2 - height / 20, width / 16, height / 10);
        }
    }

    protected override void OnFormClosed(FormClosedEventArgs e)
    {
        if (registered) UnregisterHotKey(Handle, HotkeyId);
        base.OnFormClosed(e);
    }

    [STAThread]
    private static void Main()
    {
        SetProcessDPIAware();
        Application.EnableVisualStyles();
        Application.Run(new LatencyTarget());
    }
}
