using System.Text.Json;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Layout;
using Avalonia.Media;

namespace RemoteOps.Desktop;

/// <summary>只保存公开连接参数。人工凭据由环境或本次输入提供。</summary>
internal sealed record Profile(string relay="", string server="", string ca="", string owner="", string? productName=null)
{
    private static string UserPath => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),"RemoteOps","human-desktop.json");
    private const string ProductResource = "RemoteOps.Desktop.ProductProfile.json";
    internal static bool IsManaged => typeof(Profile).Assembly.GetManifestResourceInfo(ProductResource) != null;

    public static Profile Load()
    {
        using var product = typeof(Profile).Assembly.GetManifestResourceStream(ProductResource);
        if (product != null)
        {
            var preset = JsonSerializer.Deserialize<Profile>(product);
            if (preset == null || string.IsNullOrWhiteSpace(preset.productName) ||
                string.IsNullOrWhiteSpace(preset.relay) || string.IsNullOrWhiteSpace(preset.server))
                throw new InvalidDataException("定制包缺少产品名称或 Relay 连接配置，请重新安装");
            return preset;
        }
        foreach (var path in new[] { UserPath, Path.Combine(AppContext.BaseDirectory,"connection.json") })
            if (File.Exists(path)) return JsonSerializer.Deserialize<Profile>(File.ReadAllText(path)) ?? new();
        return new();
    }
    public void Save()
    {
        Directory.CreateDirectory(Path.GetDirectoryName(UserPath)!);
        var temp = UserPath + "." + Guid.NewGuid().ToString("N");
        try { File.WriteAllText(temp,JsonSerializer.Serialize(this)); File.Move(temp,UserPath,true); }
        finally { if (File.Exists(temp)) File.Delete(temp); }
    }

    public static Task<(Profile, string)?> Edit(Window parent, Profile current)
    {
        var dialog = new Window
        {
            Title = "连接设置",
            Width = 520,
            Height = 610,
            MinWidth = 400,
            MinHeight = 460,
            WindowStartupLocation = WindowStartupLocation.CenterOwner,
        };

        var root = new Grid { RowDefinitions = new RowDefinitions("Auto,*,Auto") };

        // Header
        var header = new Border { Classes = { "dialog-header" } };
        var headerStack = new StackPanel { Spacing = 3 };
        headerStack.Children.Add(new TextBlock { Text = "Relay 连接设置", FontSize = 18, FontWeight = FontWeight.SemiBold });
        headerStack.Children.Add(new TextBlock { Text = "公开连接参数将保存到本机配置，人工凭据仅在当前会话内存中生效。", Classes = { "muted" }, FontSize = 12.5 });
        header.Child = headerStack;
        root.Children.Add(header);

        // Form Fields
        var panel = new StackPanel { Margin = new Thickness(24, 20), Spacing = 14 };
        TextBox Field(string label, string value, string watermark, bool mono = false)
        {
            var group = new StackPanel { Spacing = 6 };
            group.Children.Add(new TextBlock { Text = label, FontSize = 13.5, FontWeight = FontWeight.Medium });
            var field = new TextBox { Text = value, Watermark = watermark };
            if (mono) field.FontFamily = new FontFamily("Cascadia Code, Consolas, Microsoft YaHei UI");
            group.Children.Add(field);
            panel.Children.Add(group);
            return field;
        }
        var relay = Field("Relay 地址（主机和端口）", current.relay, "例如 relay.example.com:7443", mono: true);
        var server = Field("TLS 服务名", current.server, "证书对应的域名或 IP", mono: true);
        var ca = Field("信任证书路径（留空使用系统信任库）", current.ca, "可选，自签名或私有 CA 证书路径", mono: true);
        var owner = Field("Owner UUID（留空沿用本机设置）", current.owner, "与 Relay / MCP 保持一致的 Owner ID", mono: true);
        var token = Field("本次人工凭据（留空沿用本机环境变量）", "", "仅用于本次连接，不落盘保存");
        token.PasswordChar = '●';

        var scroll = new ScrollViewer { Content = panel };
        Grid.SetRow(scroll, 1);
        root.Children.Add(scroll);

        // Footer Actions
        var footer = new Border { Classes = { "dialog-footer" } };
        Grid.SetRow(footer, 2);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right, Spacing = 10 };
        var cancel = new Button { Content = "取消" };
        cancel.Click += (_, _) => dialog.Close(null);
        var save = new Button { Content = "保存并连接", Classes = { "primary" } };
        save.Click += (_, _) => dialog.Close(((new Profile(relay.Text?.Trim() ?? "", server.Text?.Trim() ?? "", ca.Text?.Trim() ?? "", owner.Text?.Trim() ?? "")), token.Text ?? ""));
        actions.Children.Add(cancel);
        actions.Children.Add(save);
        footer.Child = actions;
        root.Children.Add(footer);

        dialog.Content = root;
        return dialog.ShowDialog<(Profile, string)?>(parent);
    }
}
