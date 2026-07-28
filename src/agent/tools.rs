use crate::agent::utils::compare_mtime;
use crate::tui::event::{AppEvent, EventTx, QuestionInfo, QuestionOption};
use async_openai::types::chat::{ChatCompletionTool, ChatCompletionTools, FunctionObject};
use grep::regex::RegexMatcher;
use grep::searcher::Searcher;
use grep::searcher::sinks::UTF8;
use ignore::WalkBuilder;
use indoc::indoc;
use mdka::html_to_markdown;
use reqwest::Client;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

type ToolFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(String, Option<String>), ToolExecuteError>> + Send + 'a>>;

#[derive(Debug)]
pub struct ToolExecuteError {
    pub message: String,
}

impl From<std::io::Error> for ToolExecuteError {
    fn from(error: std::io::Error) -> Self {
        ToolExecuteError {
            message: error.to_string(),
        }
    }
}

impl From<reqwest::Error> for ToolExecuteError {
    fn from(error: reqwest::Error) -> Self {
        ToolExecuteError {
            message: error.to_string(),
        }
    }
}

/// 工具 trait，所有可调用的工具都需要实现它
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;
    fn execute(&self, arguments: &str) -> Result<String, ToolExecuteError>;

    /// 异步执行，默认委托给同步 `execute`。
    /// 需要真正中断能力的工具（bash、web_fetch、MCP）覆写此方法。
    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        let result = self.execute(arguments);
        Box::pin(std::future::ready(result))
    }

    /// 异步执行并返回用于 UI 展示的额外数据（如 diff）。
    /// 默认委托给 `execute_async`，display 返回 `None`。
    /// 仅需要提供 UI 专用展示信息的工具（如 edit/write）覆写此方法。
    fn execute_async_with_display<'a>(&'a self, arguments: &'a str) -> ToolFuture<'a> {
        let fut = self.execute_async(arguments);
        Box::pin(async move {
            let content = fut.await?;
            Ok((content, None))
        })
    }

    /// Whether this tool is allowed in plan mode.
    fn allowed_in_plan_mode(&self) -> bool {
        true
    }

    /// Whether this tool can be interrupted by the cancel flag.
    /// 默认可中断；需要阻塞等待用户输入的工具（如 ask_user）覆写为 false，
    /// 以避免中断后对话框残留、channel 断开等状态不一致问题。
    fn cancellable(&self) -> bool {
        true
    }
}

/// 向用户提出问题并等待回答
pub struct AskTool {
    event_tx: EventTx,
}

impl AskTool {
    pub fn new(event_tx: EventTx) -> Self {
        Self { event_tx }
    }
}

impl Tool for AskTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        crate::prompts::tools::ASK_USER
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "Questions to ask",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "Complete question text"
                            },
                            "header": {
                                "type": "string",
                                "description": "Very short label for the question tab (max 30 chars)"
                            },
                            "options": {
                                "type": "array",
                                "description": "Available choices",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "description": "Display text (1-5 words, concise)"
                                        },
                                        "description": {
                                            "type": "string",
                                            "description": "Explanation of choice"
                                        }
                                    },
                                    "required": ["label", "description"]
                                }
                            }
                        },
                        "required": ["question", "header", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    fn execute(&self, arguments: &str) -> Result<String, ToolExecuteError> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| ToolExecuteError {
            message: format!("Invalid JSON parameter: {e}"),
        })?;
        let raw_questions = args["questions"]
            .as_array()
            .ok_or_else(|| ToolExecuteError {
                message: "missing 'questions' array".to_string(),
            })?;

        if raw_questions.is_empty() {
            return Err(ToolExecuteError {
                message: "'questions' must not be empty".to_string(),
            });
        }

        let questions: Vec<QuestionInfo> = raw_questions
            .iter()
            .map(|q| {
                let question = q["question"]
                    .as_str()
                    .unwrap_or("Please enter your response:")
                    .to_string();
                let header = q["header"].as_str().unwrap_or("Question").to_string();
                let options = q["options"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .map(|o| QuestionOption {
                                label: o["label"].as_str().unwrap_or("").to_string(),
                                description: o["description"].as_str().unwrap_or("").to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                QuestionInfo {
                    question,
                    header,
                    options,
                }
            })
            .collect();

        let (tx, rx) = oneshot::channel::<String>();

        let _ = self.event_tx.try_send(AppEvent::AskUser {
            questions,
            response_tx: tx,
        });

        let response = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                rx.await.map_err(|_| ToolExecuteError {
                    message: "sender dropped".to_string(),
                })
            })
        })?;

        if response == "[User Cancelled]" {
            return Ok(response);
        }

        Ok(format!(
            "User has answered your questions: {response}. You can now continue with the user's answers in mind."
        ))
    }

    fn cancellable(&self) -> bool {
        false
    }
}
fn combine_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    if stdout.is_empty() {
        String::from_utf8_lossy(stderr).to_string()
    } else {
        format!("{}\n{}", stdout, String::from_utf8_lossy(stderr))
    }
}

pub struct BashTool;

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        crate::prompts::tools::BASH
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command_string": {
                    "type": "string",
                    "description": "Command to execute"
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory (defaults to current directory); use this instead of cd"
                },
                "timeout": {
                    "type": "number",
                    "description": "Timeout in seconds. The command will be terminated after timeout and a timeout message will be returned. Defaults to 120 seconds if not specified"
                }
            },
            "required": ["command_string"]
        })
    }

    #[cfg(windows)]
    fn execute(&self, arguments: &str) -> Result<String, ToolExecuteError> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| ToolExecuteError {
            message: format!("Invalid JSON parameter: {e}"),
        })?;
        let command = args["command_string"]
            .as_str()
            .ok_or_else(|| ToolExecuteError {
                message: "Missing 'command_string' parameter".to_string(),
            })?;
        let workdir = args["workdir"].as_str();

        let mut cmd = std::process::Command::new("powershell.exe");
        cmd.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            command,
        ]);
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }

        let output = cmd.output().map_err(|e| ToolExecuteError {
            message: format!("Failed to execute process: {e}"),
        })?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Ok(combine_output(&output.stdout, &output.stderr))
        }
    }

    #[cfg(not(windows))]
    fn execute(&self, arguments: &str) -> Result<String, ToolExecuteError> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| ToolExecuteError {
            message: format!("Invalid JSON parameter: {e}"),
        })?;
        let command = args["command_string"]
            .as_str()
            .ok_or_else(|| ToolExecuteError {
                message: "Missing 'command_string' parameter".to_string(),
            })?;
        let workdir = args["workdir"].as_str();

        let mut cmd = std::process::Command::new("bash");
        cmd.args(["-c", command]);

        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }

        let output = cmd.output().map_err(|e| ToolExecuteError {
            message: format!("Failed to execute process: {e}"),
        })?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Ok(combine_output(&output.stdout, &output.stderr))
        }
    }

    #[cfg(windows)]
    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        let args: Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: format!("Invalid JSON parameter: {e}"),
                })));
            }
        };
        let command = match args["command_string"].as_str() {
            Some(c) => c.to_string(),
            None => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: "Missing 'command_string' parameter".to_string(),
                })));
            }
        };
        let workdir = args["workdir"].as_str().map(|s| s.to_string());
        let timeout_secs = args["timeout"].as_u64().filter(|&s| s > 0).or(Some(120));
        Box::pin(async move {
            let mut cmd = tokio::process::Command::new("powershell.exe");
            cmd.kill_on_drop(true);
            cmd.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &command,
            ]);
            if let Some(dir) = &workdir {
                cmd.current_dir(dir);
            }
            let child = cmd
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| ToolExecuteError {
                    message: e.to_string(),
                })?;
            let output_fut = child.wait_with_output();
            let output = if let Some(secs) = timeout_secs {
                match tokio::time::timeout(Duration::from_secs(secs), output_fut).await {
                    Ok(result) => result.map_err(|e| ToolExecuteError {
                        message: e.to_string(),
                    })?,
                    Err(_) => {
                        return Ok(format!(
                            "[Command timed out ({}s), process terminated]",
                            secs
                        ));
                    }
                }
            } else {
                output_fut.await.map_err(|e| ToolExecuteError {
                    message: e.to_string(),
                })?
            };
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                Ok(combine_output(&output.stdout, &output.stderr))
            }
        })
    }

    #[cfg(not(windows))]
    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        let args: Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: format!("Invalid JSON parameter: {e}"),
                })));
            }
        };
        let command = match args["command_string"].as_str() {
            Some(c) => c.to_string(),
            None => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: "Missing 'command_string' parameter".to_string(),
                })));
            }
        };
        let workdir = args["workdir"].as_str().map(|s| s.to_string());
        let timeout_secs = args["timeout"].as_u64().filter(|&s| s > 0).or(Some(120));
        Box::pin(async move {
            let mut cmd = tokio::process::Command::new("bash");
            cmd.kill_on_drop(true);
            cmd.args(["-c", &command]);
            if let Some(dir) = &workdir {
                cmd.current_dir(dir);
            }
            let child = cmd
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| ToolExecuteError {
                    message: e.to_string(),
                })?;
            let output_fut = child.wait_with_output();
            let output = if let Some(secs) = timeout_secs {
                match tokio::time::timeout(Duration::from_secs(secs), output_fut).await {
                    Ok(result) => result.map_err(|e| ToolExecuteError {
                        message: e.to_string(),
                    })?,
                    Err(_) => {
                        return Ok(format!(
                            "[Command timed out ({}s), process terminated]",
                            secs
                        ));
                    }
                }
            } else {
                output_fut.await.map_err(|e| ToolExecuteError {
                    message: e.to_string(),
                })?
            };
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                Ok(combine_output(&output.stdout, &output.stderr))
            }
        })
    }
}

pub struct ReadTool;

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
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

    fn execute(&self, arguments: &str) -> Result<String, ToolExecuteError> {
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

        let path = Path::new(file_path);
        let mut result = String::new();
        if path.is_dir() {
            let entries = fs::read_dir(path)?;
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
        } else {
            let content = fs::read_to_string(path)?;
            let content = content
                .lines()
                .skip(offset - 1)
                .take(limit)
                .collect::<Vec<_>>();
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
        }

        Ok(result)
    }
}

fn serialize_diff_data(old: Option<&str>, new: &str, file_path: &str) -> String {
    serde_json::json!({
        "old": old,
        "new": new,
        "path": file_path,
    })
    .to_string()
}

pub struct EditTool;

impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
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

    fn execute(&self, arguments: &str) -> Result<String, ToolExecuteError> {
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
        let old_string = match args["old_string"].as_str() {
            Some(old_string) => old_string,
            None => {
                return Err(ToolExecuteError {
                    message: "old_string is empty".to_string(),
                });
            }
        };
        let new_string = match args["new_string"].as_str() {
            Some(new_string) => new_string,
            None => {
                return Err(ToolExecuteError {
                    message: "new_string is empty".to_string(),
                });
            }
        };
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);
        let content = fs::read_to_string(file_path)?;

        // 检测文件行尾风格，将 old_string/new_string 的行尾统一到文件的风格，
        // 避免 LLM 传入 \n 而文件是 \r\n（或反过来）导致匹配失败
        let (old_string, new_string): (String, String) = if content.contains("\r\n") {
            (
                old_string.replace("\r\n", "\n").replace('\n', "\r\n"),
                new_string.replace("\r\n", "\n").replace('\n', "\r\n"),
            )
        } else {
            (
                old_string.replace("\r\n", "\n"),
                new_string.replace("\r\n", "\n"),
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
        fs::write(file_path, res)?;

        Ok("Edit successful".to_string())
    }

    fn execute_async_with_display<'a>(&'a self, arguments: &'a str) -> ToolFuture<'a> {
        let args: Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: e.to_string(),
                })));
            }
        };
        let file_path = match args["file_path"].as_str() {
            Some(p) => p.to_string(),
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
            Ok(("Edit successful".to_string(), Some(display)))
        })
    }
}

pub struct WriteTool;

impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
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

    fn execute(&self, arguments: &str) -> Result<String, ToolExecuteError> {
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
        let content = match args["content"].as_str() {
            Some(old_string) => old_string,
            None => {
                return Err(ToolExecuteError {
                    message: "content is empty".to_string(),
                });
            }
        };
        fs::write(file_path, content)?;

        Ok("File written successfully".to_string())
    }

    fn execute_async_with_display<'a>(&'a self, arguments: &'a str) -> ToolFuture<'a> {
        let args: Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: e.to_string(),
                })));
            }
        };
        let file_path = match args["file_path"].as_str() {
            Some(p) => p.to_string(),
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
            Ok(("File written successfully".to_string(), Some(display)))
        })
    }
}

pub struct WebFetchTool {
    client: Client,
}

impl WebFetchTool {
    pub fn new() -> WebFetchTool {
        WebFetchTool {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }
}

impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        crate::prompts::tools::WEB_FETCH
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The HTTP URL to fetch"
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "html"],
                    "description": "Return format, supports html and markdown"
                },
                "timeout": {
                    "type": "number",
                    "description": "Timeout in seconds, maximum 120s"
                }
            },
            "required": ["url"]
        })
    }

    fn execute(&self, arguments: &str) -> Result<String, ToolExecuteError> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| ToolExecuteError {
            message: format!("Invalid JSON parameter: {e}"),
        })?;
        let url = match args["url"].as_str() {
            Some(url) => url.to_string(),
            None => {
                return Err(ToolExecuteError {
                    message: "url is empty".to_string(),
                });
            }
        };
        let format = args["format"].as_str().unwrap_or("markdown");

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let response = self.client.get(&url).send().await?;
                let res = response.text().await?;

                match format.to_lowercase().as_str() {
                    "markdown" => Ok(html_to_markdown(&res)),
                    "html" => Ok(res),
                    f => Err(ToolExecuteError {
                        message: format!("Unsupported format type: {}", f),
                    }),
                }
            })
        })
    }

    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        let args: Value = match serde_json::from_str(arguments) {
            Ok(v) => v,
            Err(e) => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: format!("Invalid JSON parameter: {e}"),
                })));
            }
        };
        let url = match args["url"].as_str() {
            Some(url) => url.to_string(),
            None => {
                return Box::pin(std::future::ready(Err(ToolExecuteError {
                    message: "url is empty".to_string(),
                })));
            }
        };
        let format = args["format"].as_str().unwrap_or("markdown").to_string();

        Box::pin(async move {
            let response = self.client.get(&url).send().await?;
            let res = response.text().await?;

            match format.to_lowercase().as_str() {
                "markdown" => Ok(html_to_markdown(&res)),
                "html" => Ok(res),
                f => Err(ToolExecuteError {
                    message: format!("Unsupported format type: {}", f),
                }),
            }
        })
    }
}

pub struct GrepTool;

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        crate::prompts::tools::GREP
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to search file contents"
                },
                "path": {
                    "type": "string",
                    "description": "Directory path to search, defaults to current working directory"
                },
                "include": {
                    "type": "string",
                    "description": "Glob pattern to filter file types (e.g., \"*.js\", \"*.{ts,tsx}\")"
                }
            },
            "required": ["pattern"]
        })
    }

    fn execute(&self, arguments: &str) -> Result<String, ToolExecuteError> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| ToolExecuteError {
            message: format!("Invalid JSON parameter: {e}"),
        })?;
        let pattern = match args["pattern"].as_str() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                return Err(ToolExecuteError {
                    message: "pattern must not be empty".to_string(),
                });
            }
        };
        if pattern.len() > 500 {
            return Err(ToolExecuteError {
                message: "Regex pattern too long (max 500 characters)".to_string(),
            });
        }
        let path = args["path"].as_str().unwrap_or(".");
        let include = args["include"].as_str();

        let matcher = RegexMatcher::new(&pattern).map_err(|err| ToolExecuteError {
            message: format!("Invalid regex: {}", err),
        })?;

        let mut walk_builder = WalkBuilder::new(path);
        walk_builder.sort_by_file_path(compare_mtime);
        walk_builder.hidden(false);
        walk_builder.filter_entry(|entry| {
            !crate::agent::IGNORED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
        });
        if let Some(glob) = include {
            let mut override_builder = ignore::overrides::OverrideBuilder::new(path);
            override_builder.add(glob).map_err(|err| ToolExecuteError {
                message: format!("Invalid include pattern: {}", err),
            })?;
            let overrides = override_builder.build().map_err(|err| ToolExecuteError {
                message: format!("Failed to build include filter: {}", err),
            })?;
            walk_builder.overrides(overrides);
        }

        let mut searcher = Searcher::new();
        let mut results = String::new();
        let mut match_count = 0u64;
        const MAX_MATCHES: u64 = 100;

        for entry in walk_builder.build() {
            let entry = entry.map_err(|e| ToolExecuteError {
                message: e.to_string(),
            })?;
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }

            let file_path = entry.path().to_path_buf();
            let mut file_lines = Vec::new();
            if let Err(e) = searcher.search_path(
                &matcher,
                &file_path,
                UTF8(|lnum, line| {
                    if match_count >= MAX_MATCHES {
                        return Ok(false);
                    }
                    match_count += 1;
                    file_lines.push(format!("  Line {}: {}\n", lnum, line.trim_end()));
                    Ok(true)
                }),
            ) {
                if file_lines.is_empty() {
                    continue;
                }
                if !e.to_string().contains("invalid utf-8") {
                    results.push_str(&format!("{}: (search error: {})\n", file_path.display(), e));
                }
            }

            if !file_lines.is_empty() {
                results.push_str(&format!("{}:\n", file_path.canonicalize()?.display()));
                results.push_str(&file_lines.join(""));
            }
        }

        if results.is_empty() {
            Ok("No matches found".to_string())
        } else if match_count >= MAX_MATCHES {
            results.push_str(&format!(
                "\n... Too many results, truncated (showing at most {} matches)",
                MAX_MATCHES
            ));
            Ok(results)
        } else {
            Ok(results)
        }
    }
}

pub struct GlobTool;

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        crate::prompts::tools::GLOB
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files"
                },
                "path": {
                    "type": "string",
                    "description": "Directory path to search, defaults to current working directory"
                }
            },
            "required": ["pattern"]
        })
    }

    fn execute(&self, arguments: &str) -> Result<String, ToolExecuteError> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| ToolExecuteError {
            message: format!("Invalid JSON parameter: {e}"),
        })?;
        let pattern = match args["pattern"].as_str() {
            Some(p) => p.to_string(),
            None => {
                return Err(ToolExecuteError {
                    message: "pattern must not be empty".to_string(),
                });
            }
        };
        let path = args["path"].as_str().unwrap_or(".");

        let mut walk_builder = WalkBuilder::new(path);
        walk_builder.sort_by_file_path(compare_mtime);
        walk_builder.hidden(false);
        walk_builder.filter_entry(|entry| {
            !crate::agent::IGNORED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
        });

        let glob_matcher = globset::Glob::new(&pattern)
            .map_err(|err| ToolExecuteError {
                message: format!("Invalid glob pattern: {}", err),
            })?
            .compile_matcher();

        let mut results = String::new();
        for entry in walk_builder.build() {
            let entry = entry.map_err(|e| ToolExecuteError {
                message: e.to_string(),
            })?;
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            if glob_matcher.is_match(entry.path()) {
                results.push_str(&format!("{}\n", entry.path().canonicalize()?.display()));
            }
        }

        if results.is_empty() {
            Ok("No matching files found".to_string())
        } else {
            Ok(results)
        }
    }
}

pub struct TodoWriteTool;

impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todo_write"
    }

    fn description(&self) -> &str {
        crate::prompts::tools::TODO_WRITE
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "Task list",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {
                                "type": "string",
                                "description": "Brief description of the task"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"],
                                "description": "Task status: pending, in_progress, completed, or cancelled"
                            },
                            "priority": {
                                "type": "string",
                                "enum": ["high", "medium", "low"],
                                "description": "Priority: high, medium, or low"
                            }
                        },
                        "required": ["content", "status", "priority"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    fn execute(&self, arguments: &str) -> Result<String, ToolExecuteError> {
        let args: Value = serde_json::from_str(arguments).map_err(|e| ToolExecuteError {
            message: format!("Invalid JSON parameter: {e}"),
        })?;
        let todos = args["todos"].as_array();

        match todos {
            Some(items) if !items.is_empty() => {
                let mut result = String::from("Task list updated:\n");
                for (i, item) in items.iter().enumerate() {
                    let content = item["content"].as_str().unwrap_or("Unknown task");
                    let status = item["status"].as_str().unwrap_or("pending");
                    let priority = item["priority"].as_str().unwrap_or("medium");

                    let status_icon = match status {
                        "completed" => "✓",
                        "in_progress" => "→",
                        "cancelled" => "✗",
                        _ => "○",
                    };
                    let priority_label = match priority {
                        "high" => "[high]",
                        "low" => "[low]",
                        _ => "[med]",
                    };
                    result.push_str(&format!(
                        "  {}. {} {} {} {}\n",
                        i + 1,
                        status_icon,
                        content,
                        priority_label,
                        status
                    ));
                }
                Ok(result)
            }
            _ => Err(ToolExecuteError {
                message: "todos parameter must not be empty".to_string(),
            }),
        }
    }
}

/// 工具注册表，管理所有已注册的工具
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), Arc::from(tool));
    }

    pub fn get_arc(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// 转换为 OpenAI API 所需的 tools 格式。
    pub fn to_chat_tools_filtered(&self, plan_mode: bool) -> Vec<ChatCompletionTools> {
        self.tools
            .values()
            .filter(|tool| !plan_mode || tool.allowed_in_plan_mode())
            .map(|tool| {
                ChatCompletionTools::Function(ChatCompletionTool {
                    function: FunctionObject {
                        name: tool.name().to_string(),
                        description: Some(tool.description().to_string()),
                        parameters: Some(tool.parameters()),
                        strict: Some(false),
                    },
                })
            })
            .collect()
    }

    /// 克隆注册表（Arc 引用计数共享）
    pub fn clone_registry(&self) -> ToolRegistry {
        ToolRegistry {
            tools: self.tools.clone(),
        }
    }
}
