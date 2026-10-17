#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() -> std::process::ExitCode {
    remoteops_agent_gui::run_default()
}
