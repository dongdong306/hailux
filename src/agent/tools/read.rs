use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use indoc::indoc;
use serde_json::{Value, json};
use std::fs;

use super::{
    Tool, ToolExecuteError, ToolExecution, ToolFuture, argument_str, external_dir_of,
    external_dir_request, resolve_tool_path,
};
use crate::agent::media::Attachment;
use crate::permission::PermissionRequest;
use crate::permission::resolve_permission_path;

pub struct ReadTool;

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn permission_category(&self) -> Option<&str> {
        Some("read")
    }

    fn extract_permission(&self, arguments: &str, work_dir: &str) -> Vec<PermissionRequest> {
        let Some(file_path) = argument_str(arguments, "file_path") else {
            return Vec::new();
        };
        let canonical = resolve_permission_path(&file_path, work_dir);

        // 工作目录之外 → external_directory 询问
        if let Some(dir) = external_dir_of(&file_path, work_dir) {
            return vec![external_dir_request(
                &[dir],
                format!("Read outside working directory: {}", file_path),
            )];
        }

        // 工作目录内 → read 请求（默认放行；*.env 命中内置默认询问规则）
        vec![PermissionRequest {
            permission: "read".to_string(),
            patterns: vec![canonical.display().to_string()],
            always_patterns: vec!["*".to_string()],
            description: format!("Read file: {}", file_path),
            force_ask: false,
        }]
    }

    fn description(&self) -> &str {
        crate::prompts::tools::READ
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path of the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "Starting line number to read, 1-indexed"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read, default 2000"
                }
            },
            "required": ["file_path"]
        })
    }

    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
        work_dir: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        Box::pin(async move {
            let exec = self.execute_async_with_display(arguments, work_dir).await?;
            Ok(exec.content)
        })
    }

    fn execute_async_with_display<'a>(
        &'a self,
        arguments: &'a str,
        work_dir: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Value = serde_json::from_str(arguments).map_err(|e| ToolExecuteError {
                message: format!("Invalid JSON parameter: {e}"),
            })?;
            let file_path = match args["file_path"].as_str() {
                Some(file_path) => file_path,
                None => {
                    return Err(ToolExecuteError {
                        message: "file_path is empty".to_string(),
                    });
                }
            };
            let offset: usize = args["offset"]
                .as_u64()
                .unwrap_or(1)
                .try_into()
                .unwrap_or(1)
                .max(1);
            let limit: usize = args["limit"]
                .as_u64()
                .unwrap_or(2000)
                .try_into()
                .unwrap_or(2000);

            let resolved_path = resolve_tool_path(file_path, work_dir);
            let path = Path::new(&resolved_path);
            if path.is_dir() {
                let entries = fs::read_dir(path)?;
                let mut result = String::new();
                result.push_str(
                    format!(
                        indoc! { r#"
                    <path>{}</path>
                    <type>directory</type>
                "#},
                        path.canonicalize()?.display()
                    )
                    .as_str(),
                );
                result.push_str("<entries>\n");
                for entry in entries {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        result.push_str(
                            format!(
                                "{}{}\n",
                                entry.path().canonicalize()?.display().to_string().as_str(),
                                std::path::MAIN_SEPARATOR
                            )
                            .as_str(),
                        );
                    } else {
                        result.push_str(
                            format!(
                                "{}\n",
                                entry.path().canonicalize()?.display().to_string().as_str()
                            )
                            .as_str(),
                        );
                    }
                }
                result.push_str("</entries>");
                return Ok(ToolExecution::text(result));
            }

            // 图片文件：魔数嗅探命中 → 文本只放确认说明，图片本体作为附件返回。
            // 注意：此处不校验模型是否支持视觉，能力校验推迟到发送层。
            // 先用前 12 字节嗅探魔数（PNG/JPEG/GIF/WebP 的魔数均在前 12 字节内）：
            // 命中且超限时仅凭元数据拒绝，避免把超大文件整个读入内存后再丢弃
            let mut prefix = Vec::with_capacity(12);
            {
                use std::io::Read as _;
                fs::File::open(path)?.take(12).read_to_end(&mut prefix)?;
            }
            if crate::agent::media::sniff_image_mime(&prefix).is_some() {
                let meta = fs::metadata(path)?;
                if meta.len() > crate::agent::media::MAX_IMAGE_BYTES as u64 {
                    return Ok(ToolExecution::text(format!(
                        "Image at {} is too large ({:.1} MB, max {} MB) and was not loaded.",
                        file_path,
                        meta.len() as f64 / (1024.0 * 1024.0),
                        crate::agent::media::MAX_IMAGE_BYTES / (1024 * 1024)
                    )));
                }
            }
            let bytes = fs::read(path)?;
            if let Some(mime) = crate::agent::media::sniff_image_mime(&bytes) {
                let canonical = path.canonicalize()?.display().to_string();
                let mut result = String::new();
                result.push_str(
                    format!(
                        indoc! { r#"
                    <path>{}</path>
                    <type>image</type>
                    <mime>{}</mime>
                "#},
                        canonical, mime
                    )
                    .as_str(),
                );
                result.push_str(
                    "Image read successfully. The image content is attached for visual analysis.",
                );
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file_path.to_string());
                return Ok(ToolExecution {
                    content: result,
                    display: None,
                    attachments: vec![Attachment {
                        name: Some(file_name),
                        ..Attachment::from_bytes(mime, &bytes)
                    }],
                });
            }

            // 文本文件（非 UTF-8 二进制保持原有报错行为）
            let content = String::from_utf8(bytes).map_err(|e| ToolExecuteError {
                message: format!("Stream did not contain valid UTF-8: {e} (binary file?)"),
            })?;
            let content = content
                .lines()
                .skip(offset - 1)
                .take(limit)
                .collect::<Vec<_>>();
            let mut result = String::new();
            result.push_str(
                format!(
                    indoc! { r#"
                    <path>{}</path>
                    <type>file</type>
                "#},
                    path.canonicalize()?.display()
                )
                .as_str(),
            );
            result.push_str("<content>");
            for (line_number, line) in (offset..).zip(content) {
                result.push_str(format!("{}: {}\n", line_number, line).as_str());
            }
            result.push_str("</content>");

            Ok(ToolExecution::text(result))
        })
    }
}
