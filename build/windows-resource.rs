fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let package = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
        let mut resource = winresource::WindowsResource::new();
        // Agent 的 preview 序号同时写入数字文件版本，保证 Windows 可区分预览构建。
        if matches!(
            package.as_str(),
            "remoteops-agent" | "remoteops-agent-gui" | "remoteops-agent-service"
        ) {
            let part = |name| -> u64 {
                std::env::var(name)
                    .expect("Cargo version field is required")
                    .parse::<u16>()
                    .expect("Windows version field must fit in 16 bits")
                    .into()
            };
            let prerelease = std::env::var("CARGO_PKG_VERSION_PRE").unwrap_or_default();
            let revision = prerelease.strip_prefix("preview.").map_or(0, |number| {
                u64::from(
                    number
                        .parse::<u16>()
                        .expect("Invalid preview version number"),
                )
            });
            let version = (part("CARGO_PKG_VERSION_MAJOR") << 48)
                | (part("CARGO_PKG_VERSION_MINOR") << 32)
                | (part("CARGO_PKG_VERSION_PATCH") << 16)
                | revision;
            resource.set_version_info(winresource::VersionInfo::FILEVERSION, version);
            resource.set_version_info(winresource::VersionInfo::PRODUCTVERSION, version);
        }
        if package == "remoteops-agent-gui" {
            let icon_path = std::path::PathBuf::from(
                std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest path is required"),
            )
            .join("../../assets/brand/remoteops.ico");
            println!("cargo:rerun-if-changed={}", icon_path.display());
            resource.set_icon(icon_path.to_str().expect("Logo 路径必须是有效 UTF-8"));
        }
        resource.compile().expect("无法嵌入 Windows 版本资源");
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
            && matches!(
                package.as_str(),
                "remoteops-controller-cli" | "remoteops-controller-mcp"
            )
        {
            println!("cargo:rustc-link-arg=/STACK:8388608");
        }
    }
}
