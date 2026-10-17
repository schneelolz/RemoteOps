fn main() {
    remoteops_build::emit_source_info();
    let package = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let icon = (package == "remoteops-agent-gui").then(|| {
        std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"),
        )
        .join("../../assets/brand/remoteops.ico")
    });
    remoteops_build::compile_windows_resources(&remoteops_build::WindowsResources {
        icon,
        ..remoteops_build::WindowsResources::default()
    })
    .expect("Windows resources");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
        && matches!(
            package.as_str(),
            "remoteops-controller-cli" | "remoteops-controller-mcp"
        )
    {
        println!("cargo:rustc-link-arg-bin={package}=/STACK:8388608");
    }
}
