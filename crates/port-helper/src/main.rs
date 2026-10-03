//! Narrow, authenticated macOS listener broker. Never handles files or credentials.
#[cfg(target_os = "macos")]
mod macos;
fn main() {
    if std::env::args().any(|a| a == "--version") {
        println!("transferbuddy-port-helper {}", transferbuddy_core::VERSION);
        return;
    }
    #[cfg(target_os = "macos")]
    if let Err(e) = macos::run() {
        eprintln!("Port helper: {e}");
        std::process::exit(1)
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("The port helper requires macOS");
        std::process::exit(1)
    }
}
