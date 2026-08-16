use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=web/dist");
    // 仅 web feature 开启时要求前端产物；纯 TUI 构建零 Node 依赖
    if std::env::var("CARGO_FEATURE_WEB").is_err() {
        return;
    }
    let dist = Path::new("web/dist");
    let index = dist.join("index.html");
    if !index.exists() || is_placeholder(index.to_str().unwrap_or_default()) {
        println!("cargo:warning=Building frontend (web/dist)...");
        let ok = Command::new("npm")
            .args(["run", "build"])
            .current_dir("web")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            println!(
                "cargo:warning=Failed to build frontend automatically. \
                 Run `cd web && npm install && npm run build` manually, \
                 or build with `--no-default-features` (TUI only)."
            );
        }
    }
}

/// 占位 index.html 检测（首次 clone 后 rust-embed 需要至少一个文件才能编译）
fn is_placeholder(path: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|c| c.contains("前端尚未构建"))
        .unwrap_or(false)
}
