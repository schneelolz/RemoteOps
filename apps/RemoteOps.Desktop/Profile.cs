using System.Text.Json;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Layout;

namespace RemoteOps.Desktop;

/// <summary>只保存公开连接参数。人工凭据由环境或本次输入提供。</summary>
internal sealed record Profile(string relay="", string server="", string ca="", string owner="")
{
    private static string UserPath => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),"RemoteOps","human-desktop.json");
    public static Profile Load()
    {
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
        var panel = new StackPanel { Margin = new Thickness(24), Spacing = 8 };
        var dialog = new Window { Title = "连接设置", Width = 470, Height = 530, MinWidth = 360,
            WindowStartupLocation = WindowStartupLocation.CenterOwner, Content = new ScrollViewer {Content = panel} };
        TextBox Field(string label, string value)
        {
            panel.Children.Add(new TextBlock {Text=label});
            var field = new TextBox {Text=value};
            panel.Children.Add(field);
            return field;
        }
        var relay = Field("Relay 地址（主机和端口）",current.relay);
        var server = Field("TLS 服务名",current.server);
        var ca = Field("信任证书路径（留空使用系统信任库）",current.ca);
        var owner = Field("Owner（可沿用本机设置）",current.owner);
        var token = Field("本次人工凭据（可沿用本机设置）","");
        token.PasswordChar='●';
        var save = new Button {Content="保存并连接", HorizontalAlignment=HorizontalAlignment.Right};
        save.Click += (_,_)=> dialog.Close(((new Profile(relay.Text?.Trim()??"",server.Text?.Trim()??"",ca.Text?.Trim()??"",owner.Text?.Trim()??"")),token.Text??""));
        panel.Children.Add(save);
        return dialog.ShowDialog<(Profile,string)?>(parent);
    }
}
