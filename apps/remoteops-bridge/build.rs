fn main() {
    remoteops_build::emit_source_info();
    remoteops_build::compile_windows_resources(&remoteops_build::WindowsResources {
        binary_name: Some("RemoteOpsBridge".into()),
        ..remoteops_build::WindowsResources::default()
    })
    .expect("Windows resources");
}
