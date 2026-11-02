#requires -Version 7.2
# 样式回归共享门禁：锁屏或输入桌面状态未知时拒绝，不切换或激活桌面。
if(-not ('NativeStyleDesktopGuard' -as [type])){
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Text;
public static class NativeStyleDesktopGuard {
 [DllImport("user32.dll")]static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")]static extern bool IsWindow(IntPtr window);
 [DllImport("user32.dll",SetLastError=true)]static extern uint GetWindowThreadProcessId(IntPtr window,out uint pid);
 [DllImport("kernel32.dll")]static extern uint GetCurrentThreadId();
 [DllImport("user32.dll",SetLastError=true)]static extern IntPtr GetThreadDesktop(uint threadId);
 [DllImport("user32.dll",SetLastError=true)]static extern IntPtr OpenInputDesktop(uint flags,bool inherit,uint access);
 [DllImport("user32.dll",SetLastError=true)]static extern bool CloseDesktop(IntPtr desktop);
 [DllImport("user32.dll",SetLastError=true)]static extern bool GetUserObjectInformationW(IntPtr handle,int index,out int value,uint length,out uint needed);
 [DllImport("user32.dll",EntryPoint="GetUserObjectInformationW",CharSet=CharSet.Unicode,ExactSpelling=true,SetLastError=true)]static extern bool GetDesktopName(IntPtr handle,int index,StringBuilder value,uint length,out uint needed);
 sealed class DesktopProbe {
  public bool ThreadInputKnown,ThreadInput,InputOpened,InputKnown,Input,NamesKnown,NamesMatch,InputClosed;
  public int ThreadError,ThreadInputError,InputOpenError,InputQueryError,ThreadNameError,InputNameError,CloseError;
 }
 // 纯决策入口：前台存在时维持原判据，前台为空时只接受完整、已验证的输入桌面证据。
 public static bool Decide(bool foregroundPresent,bool processQuerySucceeded,bool lockProcessSeen,bool threadInputKnown,bool threadInput,bool inputOpened,bool inputKnown,bool input,bool namesKnown,bool namesMatch,bool inputClosed){
  if(lockProcessSeen)return false;
  if(foregroundPresent)return processQuerySucceeded;
  return threadInputKnown&&threadInput&&inputOpened&&inputKnown&&input&&namesKnown&&namesMatch&&inputClosed;
 }
 // 纯重采判据：只重试空前台、已销毁句柄或已确认退出的前台进程；活窗口访问失败仍拒绝。
 public static bool RetryForeground(bool foregroundPresent,bool windowExists,bool pidQuerySucceeded,int pidError,bool processQueryFailed,bool processExited,bool lockProcessSeen){
  if(lockProcessSeen)return false;
  if(!foregroundPresent)return true;
  if(windowExists)return false;
  return (!pidQuerySucceeded&&(pidError==0||pidError==1400))||(processQueryFailed&&processExited);
 }
 // 用尽采样预算也不能把活窗口或未知故障送入输入桌面回退。
 public static bool CanFallbackForeground(bool foregroundPresent,bool windowExists,bool verifiedTransient,bool lockProcessSeen){
  return !lockProcessSeen&&(!foregroundPresent||(!windowExists&&verifiedTransient));
 }
 static bool ReadInput(IntPtr desktop,out bool input,out int error){
  int value=0;uint needed=0;bool known=desktop!=IntPtr.Zero&&GetUserObjectInformationW(desktop,6,out value,4,out needed);
  error=desktop!=IntPtr.Zero&&!known?Marshal.GetLastWin32Error():0;input=known&&value!=0;return known&&needed==4;
 }
 static string ReadName(IntPtr desktop,out int error){
  error=0;if(desktop==IntPtr.Zero)return null;
  var name=new StringBuilder(512);uint needed;
  if(!GetDesktopName(desktop,2,name,(uint)(name.Capacity*2),out needed)){error=Marshal.GetLastWin32Error();return null;}
  return needed>0&&needed<=name.Capacity*2&&name.Length>0?name.ToString():null;
 }
 static DesktopProbe InspectDesktop(){
  var result=new DesktopProbe();var thread=GetThreadDesktop(GetCurrentThreadId());result.ThreadError=thread==IntPtr.Zero?Marshal.GetLastWin32Error():0;
  result.ThreadInputKnown=ReadInput(thread,out result.ThreadInput,out result.ThreadInputError);
  var input=OpenInputDesktop(0,false,1);result.InputOpened=input!=IntPtr.Zero;result.InputOpenError=result.InputOpened?0:Marshal.GetLastWin32Error();
  try{
   result.InputKnown=ReadInput(input,out result.Input,out result.InputQueryError);
   string threadName=ReadName(thread,out result.ThreadNameError),inputName=ReadName(input,out result.InputNameError);
   result.NamesKnown=threadName!=null&&inputName!=null;result.NamesMatch=result.NamesKnown&&String.Equals(threadName,inputName,StringComparison.OrdinalIgnoreCase);
  }finally{
   if(input!=IntPtr.Zero){result.InputClosed=CloseDesktop(input);result.CloseError=result.InputClosed?0:Marshal.GetLastWin32Error();}
  }
  return result;
 }
 sealed class Probe {
  public bool Allowed,WindowExists,ProcessExited,VerifiedTransient;
  public string Reason="foreground_zero",ProcessQuery="not_attempted",ProcessClass="unknown",ProcessErrorType="none";
  public long Foreground;
  public uint ForegroundPid,ForegroundThread;
  public int Samples,NullSamples,InvalidSamples,PidError,ProcessError;
  public long ElapsedMs;
  public DesktopProbe Desktop;
 }
 static Probe Complete(Probe result,Stopwatch watch){result.ElapsedMs=watch.ElapsedMilliseconds;return result;}
 static Probe Inspect(){
  var result=new Probe();var watch=Stopwatch.StartNew();
  for(int attempt=0;attempt<11;attempt++){
   var window=GetForegroundWindow();result.Samples++;result.Foreground=window.ToInt64();
   result.ForegroundPid=0;result.ForegroundThread=0;result.PidError=0;result.ProcessError=0;
   result.ProcessQuery="not_attempted";result.ProcessClass="unknown";result.ProcessErrorType="none";
   result.WindowExists=false;result.ProcessExited=false;result.VerifiedTransient=false;
   bool retry;
   if(window==IntPtr.Zero){
    result.NullSamples++;result.Reason="foreground_zero";
    retry=RetryForeground(false,false,false,0,false,false,false);
   }else{
    uint pid;result.ForegroundThread=GetWindowThreadProcessId(window,out pid);result.ForegroundPid=pid;
    if(pid==0||result.ForegroundThread==0){
     result.PidError=Marshal.GetLastWin32Error();result.WindowExists=IsWindow(window);result.Reason="foreground_pid_query_failed";
     retry=RetryForeground(true,result.WindowExists,false,result.PidError,false,false,false);
    }else{
     Process process=null;
     try{
      process=Process.GetProcessById((int)pid);var name=process.ProcessName;result.ProcessQuery="succeeded";
      // 一旦认出锁屏进程立即拒绝，不能因为随后窗口销毁而落到桌面回退。
      if(name.Equals("LockApp",StringComparison.OrdinalIgnoreCase)){result.ProcessClass="lockapp";result.Reason="lock_process_seen";return Complete(result,watch);}
      if(name.Equals("LogonUI",StringComparison.OrdinalIgnoreCase)){result.ProcessClass="logonui";result.Reason="logon_process_seen";return Complete(result,watch);}
      result.ProcessClass="other";
      // 读取进程信息期间窗口可能销毁，末尾重新确认同一 HWND 仍属于同一 PID。
      uint stablePid;uint stableThread=GetWindowThreadProcessId(window,out stablePid);int stableError=(stablePid==0||stableThread==0)?Marshal.GetLastWin32Error():0;
      result.WindowExists=IsWindow(window);
      if(result.WindowExists&&stablePid==pid&&stableThread!=0){
       result.Allowed=Decide(true,true,false,false,false,false,false,false,false,false,false);result.Reason="available";return Complete(result,watch);
      }
      result.Reason="foreground_changed_during_process_query";result.PidError=stableError;
      retry=RetryForeground(true,result.WindowExists,stablePid!=0&&stableThread!=0,stableError,false,false,false);
     }catch(Exception error){
      result.Reason="process_name_query_failed";result.ProcessQuery="failed";
      result.ProcessErrorType=error is Win32Exception?"Win32Exception":error is ArgumentException?"ArgumentException":error is InvalidOperationException?"InvalidOperationException":"other";
      result.ProcessError=error is Win32Exception?((Win32Exception)error).NativeErrorCode:error.HResult;
      // GetProcessById 的 ArgumentException 表示该 PID 不存在；其他异常需明确 HasExited 才可重采。
      result.ProcessExited=error is ArgumentException;
      if(!result.ProcessExited&&process!=null){try{result.ProcessExited=process.HasExited;}catch{}}
      result.WindowExists=IsWindow(window);
      retry=RetryForeground(true,result.WindowExists,true,0,true,result.ProcessExited,false);
     }finally{if(process!=null)process.Dispose();}
    }
    if(!retry)return Complete(result,watch);
    result.InvalidSamples++;
   }
   result.VerifiedTransient=retry;
   int remaining=500-(int)Math.Min(watch.ElapsedMilliseconds,500L);
   if(attempt==10||remaining<=0)break;
   System.Threading.Thread.Sleep(Math.Min(50,remaining));
  }
  if(CanFallbackForeground(result.Foreground!=0,result.WindowExists,result.VerifiedTransient,false)){
   result.Desktop=InspectDesktop();var desktop=result.Desktop;
   result.Allowed=Decide(false,false,false,desktop.ThreadInputKnown,desktop.ThreadInput,desktop.InputOpened,desktop.InputKnown,desktop.Input,desktop.NamesKnown,desktop.NamesMatch,desktop.InputClosed);
   if(result.Allowed)result.Reason=result.Foreground==0?"available_input_desktop_without_foreground":"available_input_desktop_after_destroyed_foreground";
  }
  return Complete(result,watch);
 }
 static void Field(StringBuilder text,string name,object value){text.Append(';').Append(name).Append('=').Append(Convert.ToString(value,CultureInfo.InvariantCulture));}
 static void InputInfo(StringBuilder text,string prefix,IntPtr desktop,int openError){
  Field(text,prefix+"_open",desktop!=IntPtr.Zero?1:0);Field(text,prefix+"_error",openError);
  int input=0;uint needed=0;bool known=desktop!=IntPtr.Zero&&GetUserObjectInformationW(desktop,6,out input,4,out needed);
  int queryError=desktop!=IntPtr.Zero&&!known?Marshal.GetLastWin32Error():0;
  Field(text,prefix+"_input_known",known?1:0);Field(text,prefix+"_input",known?(input!=0?1:0):-1);Field(text,prefix+"_input_error",queryError);
 }
 static string Failure(Probe result){
  var text=new StringBuilder("desktop_locked_or_unavailable");
  Field(text,"reason",result.Reason);Field(text,"foreground",result.Foreground);Field(text,"foreground_pid",result.ForegroundPid);Field(text,"foreground_thread",result.ForegroundThread);
  Field(text,"samples",result.Samples);Field(text,"null_samples",result.NullSamples);Field(text,"elapsed_ms",result.ElapsedMs);Field(text,"pid_error",result.PidError);Field(text,"window_exists",result.WindowExists?1:0);Field(text,"process_exited",result.ProcessExited?1:0);Field(text,"verified_transient",result.VerifiedTransient?1:0);Field(text,"invalid_samples",result.InvalidSamples);
  Field(text,"process_query",result.ProcessQuery);Field(text,"process_class",result.ProcessClass);Field(text,"process_error_type",result.ProcessErrorType);Field(text,"process_error",result.ProcessError);
  if(result.Desktop!=null){
   var desktop=result.Desktop;
   Field(text,"fallback_thread_input_known",desktop.ThreadInputKnown?1:0);Field(text,"fallback_thread_input",desktop.ThreadInput?1:0);Field(text,"fallback_thread_error",desktop.ThreadError);Field(text,"fallback_thread_input_error",desktop.ThreadInputError);
   Field(text,"fallback_input_open",desktop.InputOpened?1:0);Field(text,"fallback_input_open_error",desktop.InputOpenError);Field(text,"fallback_input_known",desktop.InputKnown?1:0);Field(text,"fallback_input",desktop.Input?1:0);Field(text,"fallback_input_query_error",desktop.InputQueryError);
   Field(text,"fallback_names_known",desktop.NamesKnown?1:0);Field(text,"fallback_names_match",desktop.NamesMatch?1:0);Field(text,"fallback_thread_name_error",desktop.ThreadNameError);Field(text,"fallback_input_name_error",desktop.InputNameError);
   Field(text,"fallback_input_closed",desktop.InputClosed?1:0);Field(text,"fallback_close_error",desktop.CloseError);
  }
  // 诊断再次采样不改变本次门禁结论，可识别 worker 采样后的前台恢复。
  Field(text,"foreground_after",GetForegroundWindow().ToInt64());
  uint threadId=GetCurrentThreadId();Field(text,"worker_thread",threadId);
  try{using(var process=Process.GetCurrentProcess()){Field(text,"worker_pid",process.Id);Field(text,"worker_session",process.SessionId);}}catch{Field(text,"worker_metadata_unavailable",1);}
  var threadDesktop=GetThreadDesktop(threadId);int threadError=threadDesktop==IntPtr.Zero?Marshal.GetLastWin32Error():0;
  // GetThreadDesktop 返回借用句柄，不能 CloseDesktop；新打开的输入桌面则必须关闭。
  InputInfo(text,"thread_desktop",threadDesktop,threadError);
  var inputDesktop=OpenInputDesktop(0,false,1);int inputError=inputDesktop==IntPtr.Zero?Marshal.GetLastWin32Error():0;
  try{InputInfo(text,"input_desktop",inputDesktop,inputError);}finally{if(inputDesktop!=IntPtr.Zero){bool closed=CloseDesktop(inputDesktop);Field(text,"input_desktop_closed",closed?1:0);}}
  return text.ToString();
 }
 public static bool Available(){return Inspect().Allowed;}
 public static void Check(){var result=Inspect();if(!result.Allowed)throw new InvalidOperationException(Failure(result));}
}
'@
}
function Assert-NativeStyleDesktopUnlocked { [NativeStyleDesktopGuard]::Check() }
