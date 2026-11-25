//! Image attachment infrastructure: magic-number sniffing + a unified data URL attachment carrier.
//!
//! All media flows through memory/storage as `data:<mime>;base64,<data>`;
//! degradation by model vision capability happens only "before sending to the Provider"
//! (see the build_request validation in agent.rs).

use serde::{Deserialize, Serialize};

/// A single image attachment. The `data_url` convention is `data:<mime>;base64,<data>`.
///
/// `name` is an optional source identifier (e.g. the read tool fills in the image
/// file name), used only by the agent layer to generate hint text for the model;
/// serde stays compatible with old data (defaults to None) and omits it when serializing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub mime: String,
    pub data_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Attachment {
    /// Builds an attachment from raw bytes (base64-encoded inline).
    pub fn from_bytes(mime: &str, bytes: &[u8]) -> Self {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Self {
            mime: mime.to_string(),
            data_url: format!("data:{mime};base64,{encoded}"),
            name: None,
        }
    }

    /// Whether the base64 payload in the data URL is empty (corrupted/empty-image check).
    pub fn payload_is_empty(&self) -> bool {
        data_url_payload_is_empty(&self.data_url)
    }
}

/// Upper limit on raw attachment bytes (10 MiB). After base64 encoding this is ~13.3 MiB,
/// within the request body limits of mainstream OpenAI-compatible Providers.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// Upper limit on the number of attachments per message.
pub const MAX_ATTACHMENT_COUNT: usize = 6;

/// Whether the base64 payload in a data URL is empty (no comma, or an all-whitespace payload).
pub fn data_url_payload_is_empty(data_url: &str) -> bool {
    data_url
        .rsplit_once(',')
        .map(|(_, payload)| payload.trim().is_empty())
        .unwrap_or(true)
}

/// Sniffs magic bytes to detect a supported image format and returns the MIME.
/// Supports PNG / JPEG / GIF / WebP.
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

/// Whether the MIME is a supported image type.
pub fn is_supported_image_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// Infers the image MIME from a file extension, for entry points that cannot read bytes
/// immediately (e.g. @file references).
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
            name: None,
        };
        assert!(att.payload_is_empty());
    }
}
