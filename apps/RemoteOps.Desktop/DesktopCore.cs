using System.Runtime.InteropServices;
using System.Text.Json;

namespace RemoteOps.Desktop;

/// <summary>持有一个人工控制连接。界面线程定期读取状态，视频工作线程单独读取码流。</summary>
internal sealed class DesktopCore : IDisposable
{
    private const string Library = "remoteops_human_controller";
    private readonly object gate = new();
    private IntPtr handle;

    public DesktopCore(object? profile = null)
    {
        handle = ro_start_config(JsonSerializer.Serialize(profile));
        if (handle == IntPtr.Zero) throw new InvalidOperationException("人工会话组件无法启动");
    }

    public void Post(object command)
    {
        lock (gate)
        {
            if (handle == IntPtr.Zero || ro_post(handle, JsonSerializer.Serialize(command)) != 0)
                throw new InvalidOperationException("请求未发送，请检查控制连接");
        }
    }

    public string? Poll()
    {
        lock (gate)
        {
            if (handle == IntPtr.Zero) return null;
            var value = ro_event(handle);
            if (value == IntPtr.Zero) return null;
            try { return Marshal.PtrToStringUTF8(value); }
            finally { ro_free_string(value); }
        }
    }

    public int ReadFrame(long view,byte[] buffer)
    {
        lock (gate) return handle == IntPtr.Zero ? 0 : ro_frame_for(handle, view, buffer, buffer.Length);
    }

    public void Dispose()
    {
        lock (gate)
        {
            if (handle == IntPtr.Zero) return;
            ro_stop(handle);
            handle = IntPtr.Zero;
        }
    }

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] private static extern IntPtr ro_start_config([MarshalAs(UnmanagedType.LPUTF8Str)] string profile);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] private static extern void ro_stop(IntPtr h);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] private static extern int ro_post(IntPtr h, [MarshalAs(UnmanagedType.LPUTF8Str)] string json);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] private static extern IntPtr ro_event(IntPtr h);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] private static extern void ro_free_string(IntPtr p);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] private static extern int ro_frame_for(IntPtr h, long view, [Out] byte[] dst, int capacity);
}
