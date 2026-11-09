//! Placeholder until TASK-010 adds the service: it stays running so that the
//! app's LaunchAgent registration and launchd's KeepAlive can be tested
//! (TASK-009). launchd stops it with SIGTERM.
fn main() {
    println!("formationd {} (placeholder)", env!("CARGO_PKG_VERSION"));
    loop {
        std::thread::park();
    }
}
