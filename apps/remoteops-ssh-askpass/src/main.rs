fn main() {
    if let Ok(password) = std::env::var("REMOTEOPS_SSH_PASSWORD") {
        print!("{password}");
    }
}

