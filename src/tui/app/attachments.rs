use std::time::Instant;

use super::{App, Message};
use crate::tui::event::AppEvent;
use crate::tui::input::ElementKind;

pub(super) fn read_image_attachment(
    path: &std::path::Path,
) -> Option<crate::agent::media::Attachment> {
    use crate::agent::media::{self, Attachment};
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() as usize > media::MAX_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let mime = media::sniff_image_mime(&bytes)?;
    Some(Attachment::from_bytes(mime, &bytes))
}

/// Paired dedupe against the path-injection channel: WeChat/QQ "copy image"
/// delivers one paste via two channels with different bytes (bitmap
/// re-encode vs original file), which data_url dedupe can't catch. Both
/// timings count as one gesture: probe-first within 11s, or path-first with
/// a PasteBurst-flush-scale gap (<500ms).
pub(super) fn suppress_probe_result(
    leak_probe_at: Option<Instant>,
    path_attached_at: Option<Instant>,
    now: Instant,
) -> bool {
    let (Some(leak), Some(path)) = (leak_probe_at, path_attached_at) else {
        return false;
    };
    if path >= leak {
        now.duration_since(leak) < std::time::Duration::from_secs(11)
    } else {
        leak.duration_since(path) < std::time::Duration::from_millis(500)
    }
}

impl App {
    pub(super) fn next_image_placeholder(&self) -> String {
        let base = format!("[Image {}]", self.pending_images.len() + 1);
        let duplicates = self
            .pending_images
            .iter()
            .filter(|(ph, _)| ph.contains(&base))
            .count();
        if duplicates == 0 {
            base
        } else {
            format!("{} #{}", base, duplicates + 1)
        }
    }

    pub(super) fn attach_image(&mut self, attachment: crate::agent::media::Attachment) {
        if self
            .pending_images
            .iter()
            .any(|(_, a)| a.data_url == attachment.data_url)
        {
            return;
        }
        let placeholder = self.next_image_placeholder();
        let element_text = format!("{} ", placeholder);
        self.input.insert_element(&element_text, ElementKind::Image);
        self.pending_images.push((element_text, attachment));
        self.refresh_suggestions();
    }

    /// Spawned, never awaited: syncing the event loop would freeze the UI
    /// per Ctrl+V and queue probes on key holds
    pub(super) fn spawn_clipboard_image_probe(&mut self, interactive: bool) {
        if self.image_probe_in_flight {
            return;
        }
        self.image_probe_in_flight = true;
        if !interactive {
            self.leak_probe_started_at = Some(Instant::now());
        }
        let events = self.events.0.clone();
        tokio::spawn(async move {
            let image = crate::tui::clipboard::read_clipboard_image().await;
            let image = image.filter(|img| img.bytes.len() <= crate::agent::media::MAX_IMAGE_BYTES);
            let _ = events.try_send(AppEvent::PasteImageResult { image, interactive });
        });
    }

    pub(super) fn handle_clipboard_image_result(
        &mut self,
        image: Option<crate::tui::clipboard::ClipboardImage>,
        interactive: bool,
    ) {
        self.image_probe_in_flight = false;
        let leak_probe_at = self.leak_probe_started_at.take();
        match image {
            Some(img) => {
                // See suppress_probe_result: the path channel already attached
                // the original bytes; drop this re-encoded copy
                if suppress_probe_result(leak_probe_at, self.path_image_attached_at, Instant::now())
                {
                    return;
                }
                let attachment = crate::agent::media::Attachment::from_bytes(&img.mime, &img.bytes);
                self.attach_image(attachment);
                self.render.dirty = true;
            }
            None if interactive => {
                self.messages.push(Message::Agent(
                    "剪贴板中没有可粘贴的图片（支持 PNG/JPEG/GIF/WebP，≤10MB）".to_string(),
                ));
                self.render.dirty = true;
            }
            None => {}
        }
    }

    pub(super) fn take_pending_images(&mut self) -> Vec<crate::agent::media::Attachment> {
        self.pending_images.drain(..).map(|(_, att)| att).collect()
    }

    pub(super) fn take_mention_images(&self, text: &str) -> Vec<crate::agent::media::Attachment> {
        let mut images = Vec::new();
        for (display, abs) in &self.file_picker.pending_mentions {
            if !text.contains(display.as_str()) {
                continue;
            }
            let Some(path_str) = abs.strip_prefix('@') else {
                continue;
            };
            let path = std::path::Path::new(path_str);
            if crate::agent::media::mime_from_extension(path).is_none() {
                continue;
            }
            if let Some(att) = read_image_attachment(path) {
                images.push(att);
            }
        }
        images
    }
}

#[cfg(test)]
mod tests {
    use super::read_image_attachment;
    use super::suppress_probe_result;
    use std::time::{Duration, Instant};

    #[test]
    fn read_image_attachment_sniffs_content() {
        let dir = std::env::temp_dir().join(format!("hailux_paste_img_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake.png");
        std::fs::write(&fake, b"plain text not an image").unwrap();
        assert!(read_image_attachment(&fake).is_none());

        let real = dir.join("real.png");
        let mut bytes = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(b"payload");
        std::fs::write(&real, &bytes).unwrap();
        let att = read_image_attachment(&real).unwrap();
        assert_eq!(att.mime, "image/png");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn suppress_probe_result_pairs_with_path_attach() {
        let now = Instant::now();
        let leak = now - Duration::from_millis(1500);
        let path = now - Duration::from_millis(1400); // path attach after probe start
        assert!(suppress_probe_result(Some(leak), Some(path), now));
    }

    #[test]
    fn suppress_probe_result_pairs_with_probe_after_path() {
        let now = Instant::now();
        let path = now - Duration::from_millis(300);
        let probe = now - Duration::from_millis(250); // probe after path attach
        assert!(suppress_probe_result(Some(probe), Some(path), now));
    }

    #[test]
    fn suppress_probe_result_ignores_older_path_attach() {
        let now = Instant::now();
        let path = now - Duration::from_millis(2000);
        let leak = now - Duration::from_millis(1500); // new probe after the old path attach
        assert!(!suppress_probe_result(Some(leak), Some(path), now));
    }

    #[test]
    fn suppress_probe_result_expires_after_window() {
        let now = Instant::now();
        let leak = now - Duration::from_secs(12);
        let path = now - Duration::from_secs(11);
        assert!(!suppress_probe_result(Some(leak), Some(path), now));
    }

    #[test]
    fn suppress_probe_result_requires_both_timestamps() {
        let now = Instant::now();
        let t = now - Duration::from_millis(100);
        assert!(!suppress_probe_result(None, Some(t), now));
        assert!(!suppress_probe_result(Some(t), None, now));
        assert!(!suppress_probe_result(None, None, now));
    }
}
