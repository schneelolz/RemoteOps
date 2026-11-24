using System.Runtime.InteropServices;

namespace RemoteOps.Desktop;

/// <summary>在专用线程上创建、解码和释放 H.264 会话，输出紧密排列的 BGRA 像素。</summary>
internal sealed class DesktopDecoder : IDisposable
{
    private const string Library = "remoteops_view_decoder";
    private IntPtr handle = ro_codec_new(0, 1920, 1080, 30, 6_000_000);

    public DesktopDecoder()
    {
        if (handle == IntPtr.Zero) throw new InvalidOperationException("本机 H.264 解码器初始化失败");
    }

    public bool Decode(byte[] packet, int count, byte[] bgra, out int width, out int height)
    {
        if (count < 1 || count > packet.Length) throw new ArgumentOutOfRangeException(nameof(count));
        int size = ro_decode(handle, packet, count, bgra, bgra.Length, out width, out height);
        if (size == 0) return false;
        if (size < 0 || width is < 1 or > 1920 || height is < 1 or > 1080 ||
            size != width * height * 4 || size > bgra.Length)
            throw new InvalidOperationException("收到的视频帧无法解码");
        return true;
    }

    public void Dispose()
    {
        if (handle != IntPtr.Zero) ro_codec_free(handle);
        handle = IntPtr.Zero;
    }

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] private static extern IntPtr ro_codec_new(int encode, int w, int h, int fps, int bitrate);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] private static extern void ro_codec_free(IntPtr p);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] private static extern int ro_decode(IntPtr p, byte[] packet, int count, [Out] byte[] bgra, int capacity, out int w, out int h);
}
