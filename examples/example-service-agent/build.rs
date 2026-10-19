fn main() {
    remoteops_build::compile_windows_resources(&remoteops_build::WindowsResources {
        binary_name: Some("example-service-agent".into()),
        product_name: Some("Example Service Agent".into()),
        file_description: Some("Example managed service Agent".into()),
        original_filename: Some("example-service-agent.exe".into()),
        product_version: Some(env!("CARGO_PKG_VERSION").into()),
        ..remoteops_build::WindowsResources::default()
    })
    .expect("定制服务产品资源");
}
