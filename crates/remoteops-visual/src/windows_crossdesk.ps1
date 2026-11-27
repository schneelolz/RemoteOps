# 在受监控的 STA 子进程中调用已由 Agent 锁定并校验的现场 DLL。
if ($env:REMOTEOPS_VALIDATED_GFX_DLL) {
    Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class RemoteOpsCrossDesk {
    [DllImport("kernel32",CharSet=CharSet.Unicode,SetLastError=true)]
    private static extern IntPtr LoadLibraryEx(string path,IntPtr file,uint flags);
    [DllImport("kernel32",CharSet=CharSet.Ansi,ExactSpelling=true)]
    private static extern IntPtr GetProcAddress(IntPtr module,string proc);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int Inspect(IntPtr window,[Out] byte[] buffer,int size);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int Apply(IntPtr window,int[] runtime,int count,int operation,byte[] value,int size);
    private static readonly IntPtr Module=LoadLibraryEx(Environment.GetEnvironmentVariable("REMOTEOPS_VALIDATED_GFX_DLL"),IntPtr.Zero,0x800);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int Snapshot([Out] byte[] pixels,int count,int w,int h);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int SendPointer(IntPtr window,int x,int y,int ex,int ey,int op);
    public static void Pointer(IntPtr window,int x,int y,int ex,int ey,int op) {
        var fn=(SendPointer)Marshal.GetDelegateForFunctionPointer(GetProcAddress(Module,"ro_cd_pointer"),typeof(SendPointer));
        if(fn(window,x,y,ex,ey,op)!=1)throw new InvalidOperationException("CrossDesk pointer target or desktop changed");
    }
    public static byte[] Capture(int w,int h) {
        if(w<2 || h<2 || w>8192 || h>8192 || (long)w*h>16777216)throw new InvalidOperationException("Unsupported desktop geometry");
        var fn=(Snapshot)Marshal.GetDelegateForFunctionPointer(GetProcAddress(Module,"ro_cd_snapshot"),typeof(Snapshot));
        var pixels=new byte[checked(w*h*4)];
        if(fn(pixels,pixels.Length,w,h)!=1)throw new InvalidOperationException("CrossDesk capture unavailable");
        return pixels;
    }
    public static string Tree(IntPtr window) {
        var fn=(Inspect)Marshal.GetDelegateForFunctionPointer(GetProcAddress(Module,"ro_cd_uia_tree"),typeof(Inspect));
        var buf=new byte[65536];
        int n=fn(window,buf,buf.Length);
        if(n<0 || n>=buf.Length)throw new InvalidOperationException("CrossDesk UIA observation rejected");
        return Encoding.UTF8.GetString(buf,0,n);
    }
    public static void Act(IntPtr window,int[] runtime,int action,string text) {
        var fn=(Apply)Marshal.GetDelegateForFunctionPointer(GetProcAddress(Module,"ro_cd_uia_apply"),typeof(Apply));
        var bytes=Encoding.UTF8.GetBytes(text??"");
        if(fn(window,runtime,runtime.Length,action,bytes,bytes.Length)!=1)
            throw new InvalidOperationException("CrossDesk UIA target changed or operation rejected");
    }
}
'@
}
