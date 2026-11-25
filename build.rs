use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=web/dist");
    // Frontend artifacts are only required with the web feature; pure TUI builds need no Node
    if std::env::var("CARGO_FEATURE_WEB").is_err() {
        return;
    }
    let dist = Path::new("web/dist");
    let index = dist.join("index.html");
    if !index.exists() || is_placeholder(index.to_str().unwrap_or_default()) {
        println!("cargo:warning=Building frontend (web/dist)...");
        // On Windows npm is npm.cmd; CreateProcess does not resolve .cmd, so name it explicitly
        let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let ok = Command::new(npm)
            .args(["run", "build"])
            .current_dir("web")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            // Missing frontend → rust-embed fails at compile time; panic here with actionable help
            panic!(
                "Failed to build frontend automatically. \
                 Run `cd web && npm install && npm run build` manually, \
                 or build with `--no-default-features` (TUI only)."
            );
        }
    }
}

/// Placeholder index.html detection (rust-embed needs at least one file after a fresh clone)
fn is_placeholder(path: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|c| c.contains("前端尚未构建"))
        .unwrap_or(false)
}
