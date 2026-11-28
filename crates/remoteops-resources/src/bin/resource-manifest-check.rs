//! 不启动工具的资源清单校验入口。

fn main() -> std::process::ExitCode {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("用法: resource-manifest-check <index.json>");
        return std::process::ExitCode::from(2);
    };
    let result = std::fs::read(path)
        .map_err(|_| "无法读取清单".to_owned())
        .and_then(|bytes| {
            remoteops_resources::validate_manifest(&bytes).map_err(|error| error.to_string())
        });
    match result {
        Ok(index) => {
            println!(
                "资源协议 v{}，{} 个固定候选，校验通过",
                index.schema_version,
                index.tools.len()
            );
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
