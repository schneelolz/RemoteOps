#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() -> std::process::ExitCode {
    if std::env::args().any(|arg| arg == "--build-info") {
        println!(
            "{}",
            serde_json::to_string(&remoteops_agent_gui::build_info(&example_assist::profile()))
                .unwrap()
        );
        return std::process::ExitCode::SUCCESS;
    }
    if std::env::args().any(|arg| arg == "--check-config") {
        let args = std::env::args_os().filter(|arg| arg != "--check-config");
        return match remoteops_agent_gui::resolve_config(&example_assist::profile(), args) {
            Ok(_) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error:#}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    remoteops_agent_gui::run(example_assist::profile(), example_assist::options())
}
