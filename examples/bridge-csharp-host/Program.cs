using System;
using System.IO;
using System.Net;
using System.Threading;
using System.Threading.Tasks;
using Newtonsoft.Json.Linq;

internal static class Program
{
    private static int Main(string[] args) => RunAsync(args).GetAwaiter().GetResult();
    private static async Task<int> RunAsync(string[] args)
    {
        if (args.Length != 3) { Console.Error.WriteLine("用法: BridgeHost <Bridge.exe> <grclient|udserver> <初始化配置.json>"); return 2; }
        var config = JObject.Parse(File.ReadAllText(args[2]));
        string role = args[1];
        if (role != "grclient" && role != "udserver") return 2;
        string instance = (string?)config["instanceId"] ?? Guid.NewGuid().ToString("N");
        string capability = role == "grclient" ? "client.currentUser" : "store.summary";
        config["role"] = role;
        config["instanceId"] = instance;
        config["protocolVersion"] = 1;
        var emptySchema = JObject.Parse("{\"type\":\"object\",\"additionalProperties\":false,\"properties\":{}}");
        config["host"] = new JObject {
            ["serviceId"] = role, ["serviceVersion"] = "demo-1", ["instanceId"] = instance, ["protocolVersion"] = 1,
            ["capabilities"] = new JArray(new JObject {
                ["id"] = capability, ["version"] = "1", ["description"] = "仅演示的模拟业务数据", ["readOnly"] = true,
                ["inputSchema"] = emptySchema, ["outputSchema"] = new JObject { ["type"] = "object" }
            }, new JObject {
                ["id"] = "system.ipAddresses", ["version"] = "1", ["description"] = "查询宿主本机 IP", ["readOnly"] = true,
                ["inputSchema"] = emptySchema.DeepClone(), ["outputSchema"] = new JObject { ["type"] = "object" }
            })
        };
        using (var bridge = new BridgeClient(args[0], async (request, cancellation) => {
            cancellation.ThrowIfCancellationRequested();
            if ((string?)request["instanceId"] != instance || (string?)request["version"] != "1" || !(request["arguments"] is JObject parameters) || parameters.Count != 0)
                throw new InvalidOperationException("invalid_query_target_or_arguments");
            string? method = (string?)request["capabilityId"];
            if (method == "system.ipAddresses") {
                var addresses = await Dns.GetHostAddressesAsync(Dns.GetHostName()).ConfigureAwait(false);
                cancellation.ThrowIfCancellationRequested();
                return new JObject { ["addresses"] = JArray.FromObject(Array.ConvertAll(addresses, address => address.ToString())) };
            }
            if (method != capability) throw new InvalidOperationException("capability_not_allowed");
            return role == "grclient" ? new JObject { ["userId"] = "demo-user", ["displayName"] = "模拟上机用户", ["simulated"] = true }
                : new JObject { ["storeId"] = "demo-store", ["onlineClients"] = 10, ["simulated"] = true };
        })) {
            bridge.Notification += (method, value) => {
                // 演示仅显示状态；授权码由宿主在受保护的业务通道交回 AI 管家。
                Console.WriteLine(method + ": " + (string?)value["state"]);
            };
            await bridge.RequestAsync("initialize", config, CancellationToken.None).ConfigureAwait(false);
            Console.WriteLine("已启动，回车停止。");
            Console.ReadLine();
            await bridge.StopAsync().ConfigureAwait(false);
        }
        return 0;
    }
}
