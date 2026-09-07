//! 剪贴板图片探测。
//!
//! 顺序约定：先探测图片，命中才拦截；纯文本剪贴板走终端默认
//! bracketed paste 流程（由调用方保证 probe 失败时回退文本处理）。

use base64::Engine as _;
use std::time::Duration;

/// 从剪贴板读出的一张图片
#[derive(Debug)]
pub struct ClipboardImage {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// 剪贴板探测超时：PowerShell/外部工具偶发挂起时放弃本次探测，
/// 避免 TUI 事件循环被卡死。
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// 探测剪贴板中的图片。剪贴板无图片或探测失败（含超时）时返回 None。
pub(crate) async fn read_clipboard_image() -> Option<ClipboardImage> {
    tokio::time::timeout(PROBE_TIMEOUT, read_clipboard_image_platform())
        .await
        .unwrap_or_default()
}

async fn read_clipboard_image_platform() -> Option<ClipboardImage> {
    #[cfg(windows)]
    {
        read_clipboard_image_windows().await
    }
    #[cfg(target_os = "macos")]
    {
        read_clipboard_image_macos().await
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        read_clipboard_image_linux().await
    }
}

#[cfg(windows)]
async fn read_clipboard_image_windows() -> Option<ClipboardImage> {
    // Windows：PowerShell 读剪贴板图片，依次尝试：
    //   1. FileDrop 文件引用 —— QQ/微信等 IM「复制图片」把图片包装成临时文件
    //      （如 Desktop\{GUID}.png）放入剪贴板；资源管理器复制文件同理。
    //      优先读原文件字节：与粘贴路径文本通道（handle_paste 的路径识别）
    //      拿到完全相同的数据，data_url 去重可拦截双通道重复挂图，且
    //      原图质量优于位图重编码
    //   2. [Clipboard]::GetImage() —— 截图工具写入的位图（无 FileDrop 时），
    //      重编码为 PNG 返回
    // 剪贴板无图片时脚本无输出。
    //
    // FileDrop 临时文件只读不删：生命周期归 OS/IM 管理（仍被剪贴板引用，
    // 可能还有其他消费者），删除会破坏其他程序的粘贴。
    const SCRIPT: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
try {
    $files = [System.Windows.Forms.Clipboard]::GetFileDropList()
    foreach ($p in $files) {
        $ext = [IO.Path]::GetExtension($p).ToLowerInvariant()
        if ($ext -notin '.png', '.jpg', '.jpeg', '.gif', '.webp') { continue }
        if (-not (Test-Path -LiteralPath $p)) { continue }
        $item = Get-Item -LiteralPath $p
        if ($item.Length -gt 10485760) { continue }
        $b = [IO.File]::ReadAllBytes($p)
        if ($b.Length -gt 0) { [Convert]::ToBase64String($b); exit }
    }
} catch {}
$img = [System.Windows.Forms.Clipboard]::GetImage()
if ($img) {
    $ms = New-Object System.IO.MemoryStream
    $img.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    [Convert]::ToBase64String($ms.ToArray())
}
"#;
    let exe = if which_pwsh().await {
        "pwsh.exe"
    } else {
        "powershell.exe"
    };
    let output = tokio::process::Command::new(exe)
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .kill_on_drop(true) // 探测超时被丢弃时终止子进程，避免孤儿进程累积
        .output()
        .await
        .ok()?;
    decode_base64_image(&String::from_utf8_lossy(&output.stdout))
}

/// pwsh 存在性探测结果缓存：进程级只跑一次子进程探测，
/// 避免每次粘贴图片都先花几百毫秒起 pwsh --version。
#[cfg(windows)]
static HAS_PWSH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

#[cfg(windows)]
async fn which_pwsh() -> bool {
    if let Some(cached) = HAS_PWSH.get() {
        return *cached;
    }
    let found = tokio::process::Command::new("pwsh.exe")
        .arg("--version")
        .creation_flags(0x0800_0000)
        .kill_on_drop(true)
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    let _ = HAS_PWSH.set(found);
    found
}

#[cfg(target_os = "macos")]
async fn read_clipboard_image_macos() -> Option<ClipboardImage> {
    // macOS：osascript 把剪贴板 PNG 写入临时文件再读 base64
    let tmp = std::env::temp_dir().join(format!("hailux-clip-{}.png", uuid::Uuid::new_v4()));
    let script = format!(
        "set png to (the clipboard as «class PNGf»)\nset fp to open for access POSIX file \"{}\" with write permission\nset eof fp to 0\nwrite png to fp\nclose access fp",
        tmp.display()
    );
    let output = tokio::process::Command::new("osascript")
        .args(["-e", &script])
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        let _ = tokio::fs::remove_file(&tmp).await;
        return None;
    }
    let bytes = match tokio::fs::read(&tmp).await {
        Ok(b) => b,
        Err(_) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return None;
        }
    };
    let _ = tokio::fs::remove_file(&tmp).await;
    let mime = crate::agent::media::sniff_image_mime(&bytes)?;
    Some(ClipboardImage {
        mime: mime.to_string(),
        bytes,
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
async fn read_clipboard_image_linux() -> Option<ClipboardImage> {
    // Linux：优先 wl-paste（Wayland），回退 xclip（X11）
    if let Ok(output) = tokio::process::Command::new("wl-paste")
        .args(["-t", "image/png"])
        .kill_on_drop(true)
        .output()
        .await
        && output.status.success()
        && !output.stdout.is_empty()
    {
        return Some(ClipboardImage {
            mime: "image/png".to_string(),
            bytes: output.stdout,
        });
    }
    let output = tokio::process::Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "image/png", "-o"])
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(ClipboardImage {
        mime: "image/png".to_string(),
        bytes: output.stdout,
    })
}

/// 解码剪贴板工具输出的 base64（忽略空白行），并用魔数校验真实格式。
#[cfg_attr(not(windows), allow(dead_code))]
fn decode_base64_image(stdout: &str) -> Option<ClipboardImage> {
    let b64: String = stdout.chars().filter(|c| !c.is_whitespace()).collect();
    if b64.is_empty() {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let mime = crate::agent::media::sniff_image_mime(&bytes)?;
    Some(ClipboardImage {
        mime: mime.to_string(),
        bytes,
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn decode_rejects_non_image() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"not an image");
        assert!(decode_base64_image(&b64).is_none());
    }

    #[test]
    fn decode_accepts_png() {
        let png_bytes: [u8; 9] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0];
        let b64 = base64::engine::general_purpose::STANDARD.encode(png_bytes);
        let img = decode_base64_image(&b64).unwrap();
        assert_eq!(img.mime, "image/png");
    }

    #[test]
    fn decode_empty_returns_none() {
        assert!(decode_base64_image("   \n").is_none());
    }
}
