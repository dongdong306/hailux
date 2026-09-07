//! 图片附件基础设施：魔数嗅探 + 统一的 data URL 附件载体。
//!
//! 所有媒体以 `data:<mime>;base64,<data>` 形式在内存/存储层流转，
//! 仅在「发送给 Provider 前」按模型视觉能力做降级（见 agent.rs 的 build_request 校验）。

use serde::{Deserialize, Serialize};

/// 单张图片附件。`data_url` 约定为 `data:<mime>;base64,<data>`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub mime: String,
    pub data_url: String,
}

impl Attachment {
    /// 从原始字节构造附件（base64 编码内联）。
    pub fn from_bytes(mime: &str, bytes: &[u8]) -> Self {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Self {
            mime: mime.to_string(),
            data_url: format!("data:{mime};base64,{encoded}"),
        }
    }

    /// data URL 中 base64 载荷是否为空（损坏/空图片校验）。
    pub fn payload_is_empty(&self) -> bool {
        data_url_payload_is_empty(&self.data_url)
    }
}

/// 附件原始字节上限（10 MiB）。base64 后约 13.3 MiB，
/// 在主流 OpenAI-compatible Provider 的请求体限制之内。
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// 单条消息附件数量上限。
pub const MAX_ATTACHMENT_COUNT: usize = 6;

/// data URL 中 base64 载荷是否为空（无逗号 / 载荷全空白均视为空）。
pub fn data_url_payload_is_empty(data_url: &str) -> bool {
    data_url
        .rsplit_once(',')
        .map(|(_, payload)| payload.trim().is_empty())
        .unwrap_or(true)
}

/// 通过魔数字节嗅探是否为受支持的图片格式，返回 MIME。
/// 支持 PNG / JPEG / GIF / WebP。
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    // RIFF....WEBP
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// MIME 是否为受支持的图片类型。
pub fn is_supported_image_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// 按扩展名推断图片 MIME（@文件引用等无法立即读字节的入口用）。
pub fn mime_from_extension(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_png() {
        let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
        assert_eq!(sniff_image_mime(&png), Some("image/png"));
    }

    #[test]
    fn sniffs_jpeg() {
        assert_eq!(
            sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
    }

    #[test]
    fn sniffs_gif_variants() {
        assert_eq!(sniff_image_mime(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff_image_mime(b"GIF87a...."), Some("image/gif"));
    }

    #[test]
    fn sniffs_webp() {
        let webp = [b'R', b'I', b'F', b'F', 1, 2, 3, 4, b'W', b'E', b'B', b'P'];
        assert_eq!(sniff_image_mime(&webp), Some("image/webp"));
    }

    #[test]
    fn rejects_non_image() {
        assert_eq!(sniff_image_mime(b"hello world"), None);
        assert_eq!(sniff_image_mime(&[]), None);
    }

    #[test]
    fn attachment_data_url_roundtrip() {
        let att = Attachment::from_bytes("image/png", &[0x89, b'P']);
        assert_eq!(att.mime, "image/png");
        assert!(att.data_url.starts_with("data:image/png;base64,"));
        assert!(!att.payload_is_empty());
    }

    #[test]
    fn attachment_empty_payload_detected() {
        let att = Attachment {
            mime: "image/png".to_string(),
            data_url: "data:image/png;base64,".to_string(),
        };
        assert!(att.payload_is_empty());
    }
}
