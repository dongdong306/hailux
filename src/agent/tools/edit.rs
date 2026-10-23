use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};

use super::{
    Tool, ToolExecuteError, ToolExecution, ToolFuture, argument_str, diff::serialize_diff_data,
    external_dir_of, external_dir_request, resolve_tool_path,
};
use crate::permission::PermissionRequest;
use crate::permission::resolve_permission_path;

pub struct EditTool;

impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn allowed_in_plan_mode(&self) -> bool {
        false
    }

    fn permission_category(&self) -> Option<&str> {
        Some("edit")
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
                format!("Edit outside working directory: {}", file_path),
            )];
        }

        // 工作目录内 → edit 请求（默认放行，配置规则仍可 deny）
        vec![PermissionRequest {
            permission: "edit".to_string(),
            patterns: vec![canonical.display().to_string()],
            always_patterns: vec!["*".to_string()],
            description: format!("Edit {}", file_path),
            force_ask: false,
        }]
    }

    fn description(&self) -> &str {
        crate::prompts::tools::EDIT
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path of the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The original string in the text"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement string (must differ from old_string)"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Whether to replace all occurrences of old_string"
                }
            },
            "required": ["file_path", "old_string", "new_string"]
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
        let old_string_arg = match args["old_string"].as_str() {
            Some(s) => s.to_string(),
            None => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: "old_string is empty".to_string(),
                })));
            }
        };
        let new_string_arg = match args["new_string"].as_str() {
            Some(s) => s.to_string(),
            None => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: "new_string is empty".to_string(),
                })));
            }
        };
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);

        Box::pin(async move {
            let content = tokio::fs::read_to_string(&file_path).await?;

            let (old_string, new_string): (String, String) = if content.contains("\r\n") {
                (
                    old_string_arg.replace("\r\n", "\n").replace('\n', "\r\n"),
                    new_string_arg.replace("\r\n", "\n").replace('\n', "\r\n"),
                )
            } else {
                (
                    old_string_arg.replace("\r\n", "\n"),
                    new_string_arg.replace("\r\n", "\n"),
                )
            };

            let count = content.matches(old_string.as_str()).count();
            if count == 0 {
                return Err(ToolExecuteError {
                    message: "old_string not found in content".to_string(),
                });
            }
            if !replace_all && count > 1 {
                return Err(ToolExecuteError {
                    message: format!(
                        "Found {} matches for old_string. Provide more surrounding lines in old_string to identify the correct match.",
                        count
                    ),
                });
            }

            let res = if replace_all {
                content.replace(old_string.as_str(), new_string.as_str())
            } else {
                content.replacen(old_string.as_str(), new_string.as_str(), 1)
            };

            tokio::fs::write(&file_path, &res).await?;

            let display = serialize_diff_data(Some(&content), &res, &file_path);
            Ok(ToolExecution {
                content: "Edit successful".to_string(),
                display: Some(display),
                attachments: Vec::new(),
            })
        })
    }
}
