use color_eyre::Result;
use color_eyre::eyre::Context;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPO: &str = "dongdong306/hailux";

const fn asset_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "hailux-windows-amd64.exe"
    } else if cfg!(target_os = "linux") {
        "hailux-linux-amd64"
    } else {
        "hailux-unsupported"
    }
}

// ── GitHub API 类型 ─────────────────────────────────────────

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

// ── 版本比较 ───────────────────────────────────────────────

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let v = v.trim_start_matches('v');
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

fn is_newer(remote: (u32, u32, u32), local: (u32, u32, u32)) -> bool {
    remote > local
}

// ── SHA256 校验 ────────────────────────────────────────────

fn verify_sha256(data: &[u8], expected_hex: &str) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let actual = hasher.finalize();
    let actual_hex = hex_encode(&actual);
    let expected_hash = expected_hex.split_whitespace().next().unwrap_or_default();
    actual_hex.eq_ignore_ascii_case(expected_hash)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ── 核心 ───────────────────────────────────────────────────

pub async fn run_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("当前版本: {current}");

    println!("正在检查更新...");
    let client = reqwest::Client::builder()
        .user_agent("hailux-updater")
        .timeout(std::time::Duration::from_secs(1800))
        .build()?;

    let api_url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = client
        .get(&api_url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .wrap_err("无法连接 GitHub API")?;

    if !resp.status().is_success() {
        color_eyre::eyre::bail!("GitHub API 返回错误状态: {}", resp.status());
    }

    let release: Release = resp.json().await.wrap_err("无法解析 GitHub Release 信息")?;

    let remote_version = release.tag_name.trim_start_matches('v');
    let remote_ver = parse_version(remote_version)
        .ok_or_else(|| color_eyre::eyre::eyre!("无法解析远程版本号: {remote_version}"))?;
    let local_ver = parse_version(current)
        .ok_or_else(|| color_eyre::eyre::eyre!("无法解析当前版本号: {current}"))?;

    if !is_newer(remote_ver, local_ver) {
        println!("已是最新版本。");
        return Ok(());
    }

    println!("发现新版本: {remote_version}");

    // 查找目标 asset 和对应的 sha256
    let wanted = asset_name();
    let sha256_name = format!("{wanted}.sha256");

    let binary_url = release
        .assets
        .iter()
        .find(|a| a.name == wanted)
        .map(|a| &a.browser_download_url)
        .ok_or_else(|| color_eyre::eyre::eyre!("当前平台暂无可用更新包（未找到 {wanted}）"))?;

    let sha256_url = release
        .assets
        .iter()
        .find(|a| a.name == sha256_name)
        .map(|a| &a.browser_download_url);

    // 下载二进制
    print!("下载 {wanted} ... ");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    let binary_data = client
        .get(binary_url)
        .send()
        .await
        .wrap_err("下载失败")?
        .bytes()
        .await
        .wrap_err("读取下载数据失败")?;

    println!("完成 ({} bytes)", binary_data.len());

    // SHA256 校验
    if let Some(url) = sha256_url {
        print!("校验 SHA256 ... ");
        let _ = std::io::stdout().flush();
        let sha256_text = client
            .get(url)
            .send()
            .await
            .wrap_err("下载校验文件失败")?
            .text()
            .await
            .wrap_err("读取校验文件失败")?;
        let expected = sha256_text.trim();
        if verify_sha256(&binary_data, expected) {
            println!("OK");
        } else {
            color_eyre::eyre::bail!("SHA256 校验失败，文件可能已损坏，请重试");
        }
    } else {
        eprintln!("[警告] 未找到 SHA256 校验文件，跳过校验");
    }

    // 自替换
    let current_exe = std::env::current_exe().wrap_err("无法确定当前可执行文件路径")?;

    // 如果 exe 在 target/ 目录下，可能是从源码编译的
    if current_exe
        .to_string_lossy()
        .replace('\\', "/")
        .contains("/target/")
    {
        eprintln!("[提示] 检测到从源码编译运行 (路径含 target/)。");
        eprintln!("如需更新请使用: git pull && cargo build --release");
        eprintln!("或下载预编译版本: https://github.com/{REPO}/releases/latest");
        color_eyre::eyre::bail!("不支持在源码编译目录中执行自更新");
    }

    replace_binary(&current_exe, &binary_data)?;

    println!("更新完成！新版本: {remote_version}");
    println!("请重新运行 hailux。");

    Ok(())
}

fn replace_binary(exe_path: &std::path::Path, new_data: &[u8]) -> Result<()> {
    let tmp_path = exe_path.with_extension("tmp.new");

    std::fs::write(&tmp_path, new_data)
        .wrap_err_with(|| format!("无法写入临时文件: {}", tmp_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))
            .wrap_err("无法设置临时文件权限")?;

        std::fs::rename(&tmp_path, exe_path).wrap_err_with(|| {
            format!(
                "无法替换可执行文件: {}（请检查写入权限）",
                exe_path.display()
            )
        })?;
    }

    #[cfg(windows)]
    {
        let old_path = exe_path.with_extension("exe.old");

        // 清理可能残留的 .old 文件
        let _ = std::fs::remove_file(&old_path);

        // 重命名当前 exe → .old（Windows 允许重命名运行中的 exe）
        std::fs::rename(exe_path, &old_path).wrap_err_with(|| {
            format!(
                "无法重命名当前可执行文件: {}（请检查写入权限）",
                exe_path.display()
            )
        })?;

        // 移动新文件到原路径
        if let Err(e) = std::fs::rename(&tmp_path, exe_path) {
            // 回退：将 .old 恢复
            let _ = std::fs::rename(&old_path, exe_path);
            return Err(color_eyre::eyre::eyre!("无法安装更新: {e}（已回退）"));
        }

        // 尝试删除 .old（可能被锁，下次启动时清理）
        let _ = std::fs::remove_file(&old_path);
    }

    Ok(())
}

/// 启动时清理残留的 .old 文件（仅 Windows）
#[cfg(windows)]
pub fn cleanup_old_binary() {
    if let Ok(exe_path) = std::env::current_exe() {
        let old_path = exe_path.with_extension("exe.old");
        let _ = std::fs::remove_file(&old_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_normal() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
    }

    #[test]
    fn test_parse_version_invalid() {
        assert_eq!(parse_version("v1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("abc"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn test_is_newer() {
        assert!(is_newer((1, 0, 0), (0, 9, 9)));
        assert!(is_newer((1, 2, 0), (1, 1, 9)));
        assert!(is_newer((1, 2, 3), (1, 2, 2)));
        assert!(!is_newer((1, 2, 3), (1, 2, 3)));
        assert!(!is_newer((1, 2, 2), (1, 2, 3)));
    }

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0x00]), "00");
        assert_eq!(hex_encode(&[0xff]), "ff");
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

    #[test]
    fn test_verify_sha256_ok() {
        let data = b"hello world";
        let hash = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_sha256(data, hash));
    }

    #[test]
    fn test_verify_sha256_fail() {
        assert!(!verify_sha256(b"hello world", "0000000000000000"));
    }

    #[test]
    fn test_verify_sha256_case_insensitive() {
        let data = b"hello world";
        let upper = "B94D27B9934D3E08A52E52D7DA7DABFAC484EFE37A5380EE9088F7ACE2EFCDE9";
        let lower = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_sha256(data, upper));
        assert!(verify_sha256(data, lower));
    }

    #[test]
    fn test_verify_sha256_with_filename_suffix() {
        let data = b"hello world";
        let hash = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9  hailux-windows-amd64.exe";
        assert!(verify_sha256(data, hash));
    }

    #[test]
    fn test_verify_sha256_with_binary_filename_suffix() {
        let data = b"hello world";
        let hash = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9 *hailux-linux-amd64";
        assert!(verify_sha256(data, hash));
    }

    #[test]
    fn test_verify_sha256_trim_whitespace() {
        let data = b"hello world";
        let hash = "  b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9  \n";
        assert!(verify_sha256(data, hash));
    }
}
