using System.Text.Json;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Automation;
using Avalonia.Styling;

namespace RemoteOps.Desktop;

// 保存本机的界面偏好，与连接参数及凭据独立。
internal sealed record DesktopPreferences(string Theme = "auto", HashSet<string>? ClipboardOff = null)
{
    // 窗口冒烟测试可为当前进程指定独立的偏好文件。
    internal static readonly string FilePath = AppContext.GetData("RemoteOps.Desktop.AppearancePath") as string
        ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "RemoteOps", "appearance.json");

    internal static DesktopPreferences Load()
    {
        try
        {
            return JsonSerializer.Deserialize<DesktopPreferences>(File.ReadAllText(FilePath)) ?? new();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or JsonException)
        {
            return new();
        }
    }

    internal static void Change(Func<DesktopPreferences, DesktopPreferences> update)
    {
        using var guard = new Mutex(false, "RemoteOps.HumanDesktop.Preferences");
        try { if (!guard.WaitOne(TimeSpan.FromSeconds(2))) throw new IOException("本机设置正在保存，请重试"); }
        catch (AbandonedMutexException) { }
        var temp = FilePath + "." + Guid.NewGuid().ToString("N");
        try
        {
            var next = update(Load());
            Directory.CreateDirectory(Path.GetDirectoryName(FilePath)!);
            File.WriteAllText(temp, JsonSerializer.Serialize(next));
            File.Move(temp, FilePath, true);
            Apply(next.Theme);
        }
        finally
        {
            guard.ReleaseMutex();
            if (File.Exists(temp)) File.Delete(temp);
        }
    }

    internal static void Apply(string? choice)
    {
        if (Application.Current is {} app)
            app.RequestedThemeVariant = choice switch
            {
                "light" => ThemeVariant.Light,
                "dark" => ThemeVariant.Dark,
                _ => ThemeVariant.Default
            };
    }

    internal static string RequestedTheme => Application.Current?.RequestedThemeVariant switch
    {
        var theme when theme == ThemeVariant.Light => "light",
        var theme when theme == ThemeVariant.Dark => "dark",
        _ => "auto"
    };

    internal static bool SetTheme(string theme)
    {
        try
        {
            Change(preferences => preferences with { Theme = theme });
            return true;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            Apply(theme);
            return false;
        }
    }

    internal static bool ClipboardEnabled(string device) => !(Load().ClipboardOff?.Contains(device) ?? false);

    internal static void SaveClipboard(string device, bool enabled) => Change(current =>
    {
        var off = new HashSet<string>(current.ClipboardOff ?? []);
        if (enabled) off.Remove(device); else off.Add(device);
        return current with { ClipboardOff = off };
    });
}

/// <summary>从任意控制端窗口设置全应用的外观。</summary>
public sealed class AppearanceButton : Button
{
    /// <inheritdoc/>
    protected override Type StyleKeyOverride => typeof(Button);

    /// <summary>创建三种主题选择。</summary>
    public AppearanceButton()
    {
        Content = "外观";
        AutomationProperties.SetName(this, "外观主题");
        ToolTip.SetTip(this, "选择自动、浅色或深色外观");
        var menu = new MenuFlyout();
        var choices = new Dictionary<string, MenuItem>();
        foreach (var (id, label) in new[] { ("auto", "自动（跟随系统）"), ("light", "浅色"), ("dark", "深色") })
        {
            var item = new MenuItem { Header = label, ToggleType = MenuItemToggleType.Radio };
            item.Click += (_, _) =>
            {
                var saved = DesktopPreferences.SetTheme(id);
                ToolTip.SetTip(this, saved ? "选择自动、浅色或深色外观" : "本次已切换，偏好写入失败");
            };
            choices.Add(id, item);
            menu.Items.Add(item);
        }
        menu.Opening += (_, _) =>
        {
            var selected = DesktopPreferences.RequestedTheme;
            foreach (var (id, item) in choices) item.IsChecked = id == selected;
        };
        Flyout = menu;
    }
}
