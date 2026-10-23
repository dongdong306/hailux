// 图片附件：读取/挂载/占位符/剪贴板探测/提交时内联。
// 自由函数 read_image_attachment / suppress_probe_result 供粘贴路径转附件与
// @提及内联共用。
use std::time::Instant;

use super::{App, Message};
use crate::tui::event::AppEvent;
use crate::tui::input::ElementKind;

/// 读取图片文件为附件（≤10MB + 魔数校验：扩展名是图片但内容不是则拒绝）。
/// 供粘贴路径转附件与 @提及内联共用。
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

/// 泄漏探测结果是否应被丢弃（与路径注入通道配对去重）：
/// 微信/QQ「复制图片」同时携带位图与 FileDrop，同一次粘贴双通道都会挂图，
/// 两份数据字节不同（位图重编码 vs 原文件），data_url 去重无法识别。
/// 两种时序都视为同一次手势：
/// - 探测先发起（Release 泄漏）、路径后挂图：结果在探测超时窗口（10s+余量）内
/// - 路径先挂图、探测后发起（`\e[200~path\e[201~` 按键流被 Esc 拆片，尾部
///   `[201~` 触发空 paste 探测）：间隔仅在 PasteBurst flush 级别（<500ms）
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
    /// 生成下一个图片占位符基础文本（`[Image N]`）。
    /// 同名占位符已存在时追加 ` #k` 去重（仿大文本粘贴）。
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

    /// 把一张图片挂到输入框：插入占位符元素并记录附件。
    /// 占位符字面量会随文本一起提交（占位符进入 user text part）。
    pub(super) fn attach_image(&mut self, attachment: crate::agent::media::Attachment) {
        // 去重：路径文本转附件与剪贴板探测可能对同一张图双通道命中
        if self
            .pending_images
            .iter()
            .any(|(_, a)| a.data_url == attachment.data_url)
        {
            return;
        }
        let placeholder = self.next_image_placeholder();
        // 尾随空格与其他元素（大文本粘贴/@提及）惯例一致，避免后续输入紧贴 ]
        let element_text = format!("{} ", placeholder);
        self.input.insert_element(&element_text, ElementKind::Image);
        self.pending_images.push((element_text, attachment));
        self.refresh_suggestions();
    }

    /// 发起剪贴板图片探测（异步分离执行）：子进程探测可能耗时 0.5~2s
    /// （超时 10s），不能在事件循环内同步 await，否则每次 Ctrl+V 冻结 UI、
    /// 长按会排队大量探测。结果经 `AppEvent::PasteImageResult` 回传挂载；
    /// in-flight 期间忽略新请求（配合 300ms Release 去重抑制重复探测）。
    pub(super) fn spawn_clipboard_image_probe(&mut self, interactive: bool) {
        if self.image_probe_in_flight || self.is_processing {
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

    /// 处理剪贴板探测结果：命中挂载图片占位符；交互模式下未命中给轻量提示，
    /// 静默模式（Release 泄漏/空 bracketed paste）未命中无感。
    pub(super) fn handle_clipboard_image_result(
        &mut self,
        image: Option<crate::tui::clipboard::ClipboardImage>,
        interactive: bool,
    ) {
        self.image_probe_in_flight = false;
        let leak_probe_at = self.leak_probe_started_at.take();
        match image {
            Some(img) => {
                // 双通道去重：微信/QQ「复制图片」同时携带位图与 FileDrop，同一
                // 次 Ctrl+V 的路径注入通道（handle_paste 的文件路径识别）已按
                // 原文件字节挂图；探测走 GetImage 拿到的是位图重编码副本，字节
                // 不同、data_url 去重无法识别 → 按配对时间窗丢弃
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

    /// 取走全部待发送图片附件（提交时调用）
    pub(super) fn take_pending_images(&mut self) -> Vec<crate::agent::media::Attachment> {
        self.pending_images.drain(..).map(|(_, att)| att).collect()
    }

    /// 扫描文本中实际存在的 @提及，把其中的图片文件读为附件（提交时内联）。
    /// @文本被用户删除 → 不附；非图片/超大/
    /// 内容与扩展名不符 → 不附（只发路径，模型 read 会得到相应提示，自洽）。
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

    /// read_image_attachment：魔数校验兜底（扩展名是图片但内容不是 → None）
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

    /// 双通道去重：探测发起后路径通道挂图（微信/QQ 复制图片同时携带
    /// 位图与 FileDrop）→ 探测结果丢弃
    #[test]
    fn suppress_probe_result_pairs_with_path_attach() {
        let now = Instant::now();
        let leak = now - Duration::from_millis(1500);
        let path = now - Duration::from_millis(1400); // 路径挂图晚于探测发起
        assert!(suppress_probe_result(Some(leak), Some(path), now));
    }

    /// 反向时序：路径挂图在先、探测后发起（`\e[200~path\e[201~` 被 Esc
    /// 拆片，尾部空 paste 触发探测）→ 同样抑制
    #[test]
    fn suppress_probe_result_pairs_with_probe_after_path() {
        let now = Instant::now();
        let path = now - Duration::from_millis(300);
        let probe = now - Duration::from_millis(250); // 探测发起晚于路径挂图
        assert!(suppress_probe_result(Some(probe), Some(path), now));
    }

    /// 路径挂图早于本次探测（两次独立手势）→ 不抑制
    #[test]
    fn suppress_probe_result_ignores_older_path_attach() {
        let now = Instant::now();
        let path = now - Duration::from_millis(2000);
        let leak = now - Duration::from_millis(1500); // 新探测晚于上次路径挂图
        assert!(!suppress_probe_result(Some(leak), Some(path), now));
    }

    /// 探测已超出超时窗口（11s）→ 不抑制（视为独立手势）
    #[test]
    fn suppress_probe_result_expires_after_window() {
        let now = Instant::now();
        let leak = now - Duration::from_secs(12);
        let path = now - Duration::from_secs(11);
        assert!(!suppress_probe_result(Some(leak), Some(path), now));
    }

    /// 缺少任一时间戳 → 不抑制
    #[test]
    fn suppress_probe_result_requires_both_timestamps() {
        let now = Instant::now();
        let t = now - Duration::from_millis(100);
        assert!(!suppress_probe_result(None, Some(t), now));
        assert!(!suppress_probe_result(Some(t), None, now));
        assert!(!suppress_probe_result(None, None, now));
    }
}
