use std::io::{self, BufRead, Write};

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("cli") {
        println!("{}", args.get(1).map(String::as_str).unwrap_or(""));
        eprintln!("fixture-stderr");
        std::process::exit(7);
    }
    match args.first().map(String::as_str) {
        Some("timeout") => std::thread::sleep(std::time::Duration::from_secs(20)),
        Some("stderr-flood") => {
            for _ in 0..20000 { eprintln!("fixture diagnostic line"); }
            eprintln!("fixture diagnostic tail");
        }
        Some("check-agent") => {
            assert_eq!(args.len(), 1, "unexpected browser flags");
            assert_eq!(std::env::var_os("AGENT_BROWSER_EXECUTABLE_PATH"), Some(std::env::current_exe().unwrap().into_os_string()));
        }
        Some("exit" | "invalid-json" | "upstream-error") | None => {},
        _ => std::process::exit(91),
    }
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        // 先读初始化请求，固定断开阶段，避免与控制端写入发生竞态。
        match args.first().map(String::as_str) {
            Some("exit") => {
                eprintln!("fixture startup failed C:\\private\\tool.exe");
                eprintln!("token=must-not-leak");
                std::process::exit(23);
            }
            Some("invalid-json") => {
                println!("not JSON: must-not-leak");
                return;
            }
            _ => {}
        }
        let Some(start) = line.find("\"id\":") else { continue };
        let id: String = line[start + 5..].chars().take_while(char::is_ascii_digit).collect();
        if args.first().map(String::as_str) == Some("upstream-error") {
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":-32602,\"message\":\"must-not-leak\"}}}}");
            io::stdout().flush().unwrap();
            continue;
        }
        let result = if line.contains("\"method\":\"initialize\"") {
            r#"{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}"#
        } else if line.contains("\"method\":\"tools/list\"") {
            r#"{"tools":[{"name":"fixture_echo","description":"generic MCP","inputSchema":{"type":"object"}}]}"#
        } else if line.contains("\"slow\":true") {
            std::thread::sleep(std::time::Duration::from_secs(20));
            r#"{"content":[{"type":"text","text":"completed"}]}"#
        } else {
            r#"{"content":[{"type":"text","text":"generic MCP result"}]}"#
        };
        println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}");
        io::stdout().flush().unwrap();
    }
}
