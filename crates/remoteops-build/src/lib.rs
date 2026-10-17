//! Build-time product resources. All paths belong to the calling product.
use std::{env, path::PathBuf, process::Command};
#[derive(Default)]
pub struct WindowsResources {
    pub binary_name: Option<String>,
    pub icon: Option<PathBuf>,
    pub product_name: Option<String>,
    pub file_description: Option<String>,
    pub original_filename: Option<String>,
    pub product_version: Option<String>,
}
/// Embed resources in the caller's executable, never in downstream library consumers.
/// # Errors
/// Returns missing metadata, invalid version, and resource compiler errors.
pub fn compile_windows_resources(options: &WindowsResources) -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=Cargo.toml");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    let mut resource = winresource::WindowsResource::new();
    for (key, value) in [
        ("ProductName", &options.product_name),
        ("FileDescription", &options.file_description),
        ("OriginalFilename", &options.original_filename),
    ] {
        if let Some(value) = value {
            resource.set(key, value);
        }
    }
    let version = options
        .product_version
        .clone()
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap_or_default());
    resource
        .set("ProductVersion", &version)
        .set("FileVersion", &version);
    let number = windows_version(&version)?;
    resource.set_version_info(winresource::VersionInfo::FILEVERSION, number);
    resource.set_version_info(winresource::VersionInfo::PRODUCTVERSION, number);
    if let Some(path) = &options.icon {
        println!("cargo:rerun-if-changed={}", path.display());
        resource.set_icon(
            path.to_str()
                .ok_or_else(|| std::io::Error::other("Icon path must be UTF-8"))?,
        );
    }
    let binary = options
        .binary_name
        .clone()
        .unwrap_or_else(|| env::var("CARGO_PKG_NAME").unwrap_or_default());
    resource.compile_for(&binary)
}
fn windows_version(version: &str) -> std::io::Result<u64> {
    let (release, pre) = version.split_once('-').unwrap_or((version, ""));
    let mut value = 0_u64;
    let mut parts = release.split('.');
    for _ in 0..3 {
        let part = parts
            .next()
            .unwrap_or("0")
            .parse::<u16>()
            .map_err(std::io::Error::other)?;
        value = (value << 16) | u64::from(part);
    }
    let revision = pre
        .strip_prefix("preview.")
        .unwrap_or("0")
        .parse::<u16>()
        .map_err(std::io::Error::other)?;
    Ok((value << 16) | u64::from(revision))
}
/// Record the actual dependency checkout; archives explicitly report unknown provenance.
pub fn emit_source_info() {
    let root = env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default();
    let git = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .current_dir(&root)
            .args(args)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let commit = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain"])
        .map_or("unknown", |s| if s.is_empty() { "false" } else { "true" });
    println!("cargo:rustc-env=REMOTEOPS_SOURCE_COMMIT={commit}");
    println!("cargo:rustc-env=REMOTEOPS_SOURCE_DIRTY={dirty}");
    if let Some(top) = git(&["rev-parse", "--show-toplevel"]) {
        if let Some(files) = git(&["ls-files", "--full-name"]) {
            for file in files.lines() {
                println!("cargo:rerun-if-changed={top}/{file}");
            }
        }
        for name in ["HEAD", "index", "refs"] {
            println!("cargo:rerun-if-changed={top}/.git/{name}");
        }
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn version_includes_preview() {
        assert_eq!(
            super::windows_version("1.2.3-preview.4").unwrap(),
            (1 << 48) | (2 << 32) | (3 << 16) | 4
        );
        assert!(super::windows_version("999999.0.0").is_err());
    }
}
