use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};

use super::{
    Tool, ToolExecuteError, ToolExecution, ToolFuture, argument_str, diff::serialize_diff_data,
    external_dir_of, external_dir_request, resolve_tool_path,
};
use crate::permission::PermissionRequest;
use crate::permission::resolve_permission_path;

pub struct WriteTool;

impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn allowed_in_plan_mode(&self) -> bool {
        false
    }

    fn permission_category(&self) -> Option<&str> {
        Some("write")
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
                format!("Write outside working directory: {}", file_path),
            )];
        }

        // 工作目录内 → write 请求（默认放行，配置规则仍可 deny）
        vec![PermissionRequest {
            permission: "write".to_string(),
            patterns: vec![canonical.display().to_string()],
            always_patterns: vec!["*".to_string()],
            description: format!("Write {}", file_path),
            force_ask: false,
        }]
    }

    fn description(&self) -> &str {
        crate::prompts::tools::WRITE
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path of the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "The string content to write"
                }
            },
            "required": ["file_path", "content"]
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
        let args: Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: e.to_string(),
                })));
            }
        };
        let file_path = match args["file_path"].as_str() {
            Some(p) => resolve_tool_path(p, work_dir),
            None => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: "file_path is empty".to_string(),
                })));
            }
        };
        let content = match args["content"].as_str() {
            Some(s) => s.to_string(),
            None => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: "content is empty".to_string(),
                })));
            }
        };

        Box::pin(async move {
            let old_content = tokio::fs::read_to_string(&file_path).await.ok();

            tokio::fs::write(&file_path, &content).await?;

            let display = serialize_diff_data(old_content.as_deref(), &content, &file_path);
            Ok(ToolExecution {
                content: "File written successfully".to_string(),
                display: Some(display),
                attachments: Vec::new(),
            })
        })
    }
}
