using System;
using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Newtonsoft.Json;
using Newtonsoft.Json.Linq;

/// <summary>演示 C# 启动并双向调用 Bridge，兼容 .NET Framework 4.6.2。</summary>
public sealed class BridgeClient : IDisposable
{
    private const int MaxBytes = 256 * 1024;
    private readonly Process process;
    private readonly StreamWriter input;
    private readonly SemaphoreSlim writeLock = new SemaphoreSlim(1);
    private readonly SemaphoreSlim concurrency = new SemaphoreSlim(16);
    private readonly ConcurrentDictionary<string, TaskCompletionSource<JToken>> pending = new ConcurrentDictionary<string, TaskCompletionSource<JToken>>();
    private readonly ConcurrentDictionary<string, CancellationTokenSource> calls = new ConcurrentDictionary<string, CancellationTokenSource>();
    private readonly Func<JObject, CancellationToken, Task<JToken>> invoke;
    private readonly Task reader;
    private readonly Task errors;
    private long sequence;
    private bool disposed;

    /// <summary>接收状态、控制码与脱敏错误；生产宿主不得将控制码写入普通日志。</summary>
    public event Action<string, JToken>? Notification;

    /// <summary>启动子进程，不使用命令行传递凭据。</summary>
    public BridgeClient(string executable, Func<JObject, CancellationToken, Task<JToken>> invoke)
    {
        this.invoke = invoke;
        process = new Process { StartInfo = new ProcessStartInfo(Path.GetFullPath(executable)) {
            UseShellExecute = false, CreateNoWindow = true, RedirectStandardInput = true,
            RedirectStandardOutput = true, RedirectStandardError = true,
            StandardOutputEncoding = new UTF8Encoding(false, true), StandardErrorEncoding = Encoding.UTF8
        }};
        process.Start();
        input = new StreamWriter(process.StandardInput.BaseStream, new UTF8Encoding(false, true)) { NewLine = "\n" };
        reader = ReadAsync();
        // 始终排空 stderr，防止日志管道填满后子进程阻塞；示例不落盘。
        errors = DrainErrorsAsync();
    }

    /// <summary>发送一次请求，超时或取消均不自动重放。</summary>
    public async Task<JToken> RequestAsync(string method, JToken parameters, CancellationToken cancellationToken)
    {
        await concurrency.WaitAsync(cancellationToken).ConfigureAwait(false);
        string id = "host:" + Interlocked.Increment(ref sequence);
        var completion = new TaskCompletionSource<JToken>(TaskCreationOptions.RunContinuationsAsynchronously);
        try {
            if (!pending.TryAdd(id, completion)) throw new InvalidOperationException("duplicate_request");
            using (var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken)) {
                timeout.CancelAfter(TimeSpan.FromSeconds(30));
                using (timeout.Token.Register(() => completion.TrySetCanceled())) {
                    await SendAsync(new JObject { ["jsonrpc"] = "2.0", ["id"] = id, ["method"] = method, ["params"] = parameters }).ConfigureAwait(false);
                    return await completion.Task.ConfigureAwait(false);
                }
            }
        } finally { pending.TryRemove(id, out _); concurrency.Release(); }
    }

    private async Task SendAsync(JObject message)
    {
        string text = message.ToString(Formatting.None);
        if (Encoding.UTF8.GetByteCount(text) + 1 > MaxBytes) throw new InvalidDataException("message_too_large");
        await writeLock.WaitAsync().ConfigureAwait(false);
        try { await input.WriteLineAsync(text).ConfigureAwait(false); await input.FlushAsync().ConfigureAwait(false); }
        finally { writeLock.Release(); }
    }

    private async Task<string?> ReadBoundedLineAsync()
    {
        var line = new StringBuilder();
        var buffer = new char[1];
        while (await process.StandardOutput.ReadAsync(buffer, 0, 1).ConfigureAwait(false) != 0) {
            if (buffer[0] == '\n') {
                string text = line.ToString();
                if (Encoding.UTF8.GetByteCount(text) + 1 > MaxBytes) throw new InvalidDataException("message_too_large");
                return text;
            }
            if (line.Length >= MaxBytes) throw new InvalidDataException("message_too_large");
            line.Append(buffer[0]);
        }
        if (line.Length != 0) throw new EndOfStreamException("incomplete_message");
        return null;
    }

    private async Task ReadAsync()
    {
        try {
            string? line;
            while ((line = await ReadBoundedLineAsync().ConfigureAwait(false)) != null) {
                var message = JObject.Parse(line);
                if ((string?)message["jsonrpc"] != "2.0") throw new InvalidDataException("invalid_protocol");
                string? method = (string?)message["method"];
                string? id = (string?)message["id"];
                if (method == "host.invoke" && id != null) {
                    if (calls.Count >= 16) { await RejectAsync(id, "host_busy").ConfigureAwait(false); continue; }
                    var cancellation = new CancellationTokenSource(TimeSpan.FromSeconds(30));
                    if (!calls.TryAdd(id, cancellation)) { cancellation.Dispose(); throw new InvalidDataException("duplicate_request"); }
                    _ = InvokeAsync(id, (JObject)message["params"]!, cancellation);
                } else if (method == "$/cancelRequest") {
                    string? cancelledId = (string?)message["params"]?["id"];
                    if (cancelledId != null && calls.TryGetValue(cancelledId, out var cancellation)) cancellation.Cancel();
                } else if (method != null) {
                    try { Notification?.Invoke(method, message["params"] ?? JValue.CreateNull()); } catch { /* 观察者异常不能终止读循环。 */ }
                } else if (id != null && pending.TryRemove(id, out var completion)) {
                    if (message["error"] != null) completion.TrySetException(new InvalidOperationException("bridge_request_rejected"));
                    else completion.TrySetResult(message["result"] ?? JValue.CreateNull());
                }
            }
        } catch (Exception error) { FailPending(error); }
        finally {
            foreach (var call in calls.Values) call.Cancel();
            FailPending(new EndOfStreamException("bridge_closed: result_unknown"));
        }
    }

    private async Task InvokeAsync(string id, JObject request, CancellationTokenSource cancellation)
    {
        try {
            JToken result = await invoke(request, cancellation.Token).ConfigureAwait(false);
            await SendAsync(new JObject { ["jsonrpc"] = "2.0", ["id"] = id, ["result"] = result }).ConfigureAwait(false);
        } catch { try { await RejectAsync(id, "host_query_failed_or_cancelled").ConfigureAwait(false); } catch { } }
        finally { calls.TryRemove(id, out _); cancellation.Dispose(); }
    }

    private Task RejectAsync(string id, string reason) => SendAsync(new JObject { ["jsonrpc"] = "2.0", ["id"] = id, ["error"] = new JObject { ["code"] = -32000, ["message"] = reason }});
    private void FailPending(Exception error) { foreach (var entry in pending) if (pending.TryRemove(entry.Key, out var completion)) completion.TrySetException(error); }
    private async Task DrainErrorsAsync() { var buffer = new char[1024]; try { while (await process.StandardError.ReadAsync(buffer, 0, buffer.Length).ConfigureAwait(false) != 0) { } } catch (ObjectDisposedException) { } }

    /// <summary>先尝试有序退出，超时只结束当前子进程。</summary>
    public async Task StopAsync()
    {
        if (!process.HasExited) {
            try { await RequestAsync("shutdown", new JObject(), CancellationToken.None).ConfigureAwait(false); } catch { }
            input.Close();
            await Task.WhenAny(reader, Task.Delay(12000)).ConfigureAwait(false);
            if (!process.HasExited && !process.WaitForExit(1000)) process.Kill();
        }
        await Task.WhenAll(reader, errors).ConfigureAwait(false);
    }

    /// <summary>关闭管道使 Bridge 退出，释放进程句柄。</summary>
    public void Dispose()
    {
        if (disposed) return;
        disposed = true;
        try { input.Close(); } catch (InvalidOperationException) { }
        if (!process.HasExited && !process.WaitForExit(12000)) process.Kill();
        process.Dispose();
    }
}
