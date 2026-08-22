fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .compile()
            .expect("无法嵌入 Windows 版本资源");
        let package = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
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
