fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .define("NOMINMAX", None)
        .define("UNICODE", None)
        .define("_UNICODE", None);
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.flag("/utf-8").flag("/EHsc");
    }
    for source in [
        "native/codec.cpp",
        "native/host.cpp",
        "native/view.cpp",
        "native/clipboard.cpp",
        "native/clipboard_ole.cpp",
    ] {
        println!("cargo:rerun-if-changed={source}");
        build.file(source);
    }
    println!("cargo:rerun-if-changed=native/clipboard_internal.hpp");
    println!("cargo:rerun-if-changed=native/clipboard_image.hpp");
    println!("cargo:rerun-if-changed=native/crossdesk_capture.hpp");
    build.compile("remoteops_desktop");
    for lib in [
        "d3d11",
        "dxgi",
        "gdi32",
        "windowscodecs",
        "user32",
        "ole32",
        "shell32",
        "uuid",
        "oleaut32",
        "mf",
        "mfplat",
        "mfreadwrite",
        "mfuuid",
        "wmcodecdspuuid",
        "shcore",
        "wtsapi32",
        "comctl32",
        "dwmapi",
        "advapi32",
    ] {
        println!("cargo:rustc-link-lib={lib}");
    }
}
