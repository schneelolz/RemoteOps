fn main() {
    remoteops_build::emit_source_info();
    println!("cargo:rerun-if-changed=app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../assets/brand/remoteops.ico");
    winresource::WindowsResource::new()
        .set_icon(icon.to_str().expect("icon path"))
        .set_manifest(include_str!("app.manifest"))
        .compile_for("remoteops-agent-native-prototype")
        .expect("native prototype Windows resources");
}
