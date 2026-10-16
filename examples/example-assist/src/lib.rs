//! Only product-owned configuration and one small optional presentation extension.
use remoteops_agent_gui::{
    EndpointPolicy, ExtensionSlot, GuiAction, GuiExtension, GuiOptions, GuiSnapshot,
    ProductProfile, egui,
};

pub fn profile() -> ProductProfile {
    ProductProfile {
        id: "example-assist".into(),
        name: "Example Assist".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        directory_name: "ExampleAssist".into(),
        config_file_name: "example-assist.json".into(),
        state_file_name: "example-identity.json".into(),
        env_prefix: "EXAMPLE_ASSIST".into(),
        endpoint: EndpointPolicy::Fixed {
            relay: "relay.example.invalid:7443".into(),
            server_name: "relay.example.invalid".into(),
        },
        default_full_control: true,
        legacy_remoteops_paths: false,
    }
}

pub fn options() -> GuiOptions {
    GuiOptions {
        icon_png: Some(include_bytes!("../assets/example.png")),
        activity: true,
        extension: Some(Box::new(ExampleHint)),
        ..GuiOptions::default()
    }
}

struct ExampleHint;
impl GuiExtension for ExampleHint {
    fn render(
        &mut self,
        slot: ExtensionSlot,
        ui: &mut egui::Ui,
        state: &GuiSnapshot,
    ) -> Vec<GuiAction> {
        if slot != ExtensionSlot::ConnectionHint {
            return Vec::new();
        }
        ui.weak(match state.language {
            remoteops_agent_gui::extension::Language::ZhCn => {
                "示例产品 · 测试端点 · 不连接生产环境"
            }
            remoteops_agent_gui::extension::Language::EnUs => {
                "Example product · test endpoint · no production connection"
            }
        });
        if ui.small_button("Logs / 日志").clicked() {
            vec![GuiAction::OpenLogs]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(path: &std::path::Path) -> Vec<std::ffi::OsString> {
        vec![
            "example-assist".into(),
            "--config".into(),
            path.as_os_str().into(),
            "--demo".into(),
        ]
    }
    #[test]
    fn external_product_uses_public_resolver_and_preserves_explicit_false() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.json");
        for (json, expected) in [
            ("{}", true),
            (r#"{"default_full_control":false}"#, false),
            (r#"{"default_full_control":true}"#, true),
        ] {
            std::fs::write(&path, json).unwrap();
            let config = remoteops_agent_gui::resolve_config(&profile(), args(&path)).unwrap();
            assert_eq!(config.default_full_control, expected);
            assert_eq!(config.relay, "relay.example.invalid:7443");
            assert!(
                config
                    .transfer_root
                    .to_string_lossy()
                    .contains("ExampleAssist")
            );
        }
    }
    #[test]
    fn fixed_policy_rejects_files_arguments_and_environment() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.json");
        std::fs::write(&path, r#"{"relay":"other.invalid:7443"}"#).unwrap();
        assert!(remoteops_agent_gui::resolve_config(&profile(), args(&path)).is_err());
        std::fs::write(&path, "{}").unwrap();
        let mut input = args(&path);
        input.extend(["--relay".into(), "other.invalid:7443".into()]);
        assert!(remoteops_agent_gui::resolve_config(&profile(), input).is_err());
    }
    #[test]
    fn resources_and_versions_come_from_the_product_and_linked_library() {
        assert!(options().activity);
        assert_ne!(
            profile().version,
            remoteops_agent_gui::build_info(&profile()).gui_version
        );
        assert_eq!(
            remoteops_agent_gui::build_info(&profile()).protocol_version,
            16
        );
        assert!(options().icon_png.unwrap().starts_with(b"\x89PNG"));
    }
}
