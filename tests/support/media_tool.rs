//! A deliberately broken decoder used to exercise failure and cancellation paths.
use std::io::Write;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode != "--stall" {
        std::io::stdout()
            .write_all(&vec![40; 32 * 32 * 3 * 4])
            .unwrap();
        std::io::stdout().flush().unwrap();
    }
    if mode == "--stall" || mode == "--stall-after-frame" {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
    eprintln!("intentional decoder failure after partial output");
    std::process::exit(1);
}
