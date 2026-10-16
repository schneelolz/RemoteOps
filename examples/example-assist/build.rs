fn main() {
    remoteops_build::compile_windows_resources(&remoteops_build::WindowsResources {
        binary_name: Some("example-assist".into()),
        icon: Some(
            std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
                .join("assets/example.ico"),
        ),
        product_name: Some("Example Assist".into()),
        file_description: Some("Example Assist field assistance".into()),
        original_filename: Some("example-assist.exe".into()),
        product_version: Some(env!("CARGO_PKG_VERSION").into()),
    })
    .expect("product Windows resources");
}
