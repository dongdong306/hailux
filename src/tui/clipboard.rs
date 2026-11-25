//! Clipboard image detection.
//!
//! Ordering convention: probe for an image first and only intercept on a hit; a plain-text
//! clipboard goes through the terminal's default bracketed paste flow (the caller guarantees
//! a fallback to text handling when the probe fails).

use base64::Engine as _;
use std::time::Duration;

/// An image read from the clipboard
#[derive(Debug)]
pub struct ClipboardImage {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Clipboard probe timeout: give up the probe when PowerShell/external tools occasionally
/// hang, so the TUI event loop never gets stuck.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Probe the clipboard for an image. Returns None when there is no image or the probe
/// fails (incl. timeout).
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
    // Windows: PowerShell reads a clipboard image, trying in order:
    //   1. FileDrop file refs — IM apps (QQ/WeChat etc.) wrap "copy image" as a temp file
    //      (e.g. Desktop\{GUID}.png) placed in the clipboard; Explorer file copies work the same.
    //      Prefer reading the original file bytes: identical data to the paste-path text
    //      channel (handle_paste's path detection), so data_url dedup can catch duplicate
    //      attachments across both channels, and original quality beats bitmap re-encoding
    //   2. [Clipboard]::GetImage() — bitmap written by screenshot tools (when no FileDrop),
    //      re-encoded to PNG
    // The script outputs nothing when the clipboard has no image.
    //
    // FileDrop temp files are read-only, never deleted: their lifecycle is owned by the
    // OS/IM (still referenced by the clipboard, possibly with other consumers); deleting
    // them would break other programs' paste.
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
        .kill_on_drop(true) // kill the child when the timed-out probe is dropped, avoiding orphans
        .output()
        .await
        .ok()?;
    decode_base64_image(&String::from_utf8_lossy(&output.stdout))
}

/// Cache for the pwsh existence probe: run the child-process probe only once per process,
/// so each image paste doesn't first spend hundreds of ms spawning pwsh --version.
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
    // macOS: osascript writes the clipboard PNG to a temp file, then read it back as base64
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
    // Linux: prefer wl-paste (Wayland), fall back to xclip (X11)
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

/// Decode the base64 output of clipboard tools (ignoring whitespace) and verify the real
/// format via magic bytes.
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
