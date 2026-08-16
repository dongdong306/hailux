use crate::agent::event::{CoreEvent, QuestionInfo, QuestionOption};
use crate::agent::utils::compare_mtime;
use crate::permission::PermissionRequest;
use crate::permission::bash_arity::extract_bash_pattern;
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
use std::sync::OnceLock;
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

/// 词法规范化路径（解析 `.` 与 `..`），不做文件系统访问。
/// 用于 canonicalize 失败（文件不存在）时仍能正确判断「工作目录内」。
fn normalize_lexical(path: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 去掉 Windows canonicalize 返回的 `\\?\` 扩展路径前缀，
/// 保证 canonicalize 与词法回退路径都能用 `starts_with` 正确比较。
#[cfg(windows)]
fn strip_extended_prefix(path: std::path::PathBuf) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        return std::path::PathBuf::from(stripped);
    }
    path
}

#[cfg(not(windows))]
fn strip_extended_prefix(path: std::path::PathBuf) -> std::path::PathBuf {
    path
}

/// 解析权限检查用的路径。
/// 相对路径先按 work_dir 拼接并做词法规范化（解析 ..），再尝试 canonicalize
/// （解析符号链接等）；文件不存在时回退到规范化后的拼接路径。
/// 注意不能直接 canonicalize 相对路径——它会相对进程 cwd 而非 work_dir 解析。
fn resolve_permission_path(path: &str, work_dir: &str) -> std::path::PathBuf {
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        normalize_lexical(&Path::new(work_dir).join(path))
    };
    match std::fs::canonicalize(&joined) {
        Ok(c) => strip_extended_prefix(c),
        Err(_) => {
            // 文件不存在时，逐级向上找到第一个存在的祖先目录并 canonicalize，
            // 再把不存在的部分拼回去。这解决了 Windows 短路径名（如 RUNNER~1）
            // 与 canonicalize 结果不一致导致 starts_with 匹配失败的问题。
            let mut existing = joined.clone();
            let mut missing: Vec<std::ffi::OsString> = Vec::new();
            while !existing.as_os_str().is_empty() {
                match std::fs::canonicalize(&existing) {
                    Ok(canon) => {
                        let mut result = strip_extended_prefix(canon);
                        for part in missing.into_iter().rev() {
                            result.push(part);
                        }
                        return result;
                    }
                    Err(_) => {
                        if let Some(name) = existing.file_name() {
                            missing.push(name.to_os_string());
                        }
                        if !existing.pop() {
                            break;
                        }
                    }
                }
            }
            strip_extended_prefix(joined)
        }
    }
}

/// 构造「操作非工作目录内容」的权限请求（external_directory，默认询问）。
fn external_dir_request(dir: &Path, description: String) -> PermissionRequest {
    let pattern = dir.join("*").display().to_string();
    PermissionRequest {
        permission: "external_directory".to_string(),
        patterns: vec![pattern.clone()],
        always_patterns: vec![pattern],
        description,
    }
}

/// 工具 trait，所有可调用的工具都需要实现它
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;

    /// 异步执行工具。所有工具都必须实现真正的异步执行，
    /// 避免在 executor 线程上做阻塞 I/O 或绕过 timeout。
    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>>;

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

    /// 返回此工具的权限类别（如 "bash", "read", "edit", "write", "mcp"）。
    /// 返回 None 表示此工具不需要权限检查。
    fn permission_category(&self) -> Option<&str> {
        None
    }

    /// 从工具参数中提取权限检查所需的信息。
    /// 返回 None 表示不需要权限检查（即使 permission_category 返回了 Some）。
    fn extract_permission(&self, _arguments: &str, _work_dir: &str) -> Option<PermissionRequest> {
        None
    }
}

/// 向用户提出问题并等待回答
pub struct AskTool {
    event_hub: crate::agent::event::EventHub,
}

impl AskTool {
    pub fn new(event_hub: crate::agent::event::EventHub) -> Self {
        Self { event_hub }
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

    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        Box::pin(async move {
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
                                    description: o["description"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string(),
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

            let delivered = self.event_hub.send(CoreEvent::AskUser {
                questions,
                response_tx: tx,
            });
            if !delivered {
                // 通道未绑定或已关闭（如 Web 请求已断开）：
                // 立即失败，避免 agent 永久挂起等待用户回复
                return Err(ToolExecuteError {
                    message: "ask_user unavailable: no active event channel".to_string(),
                });
            }

            let response = rx.await.map_err(|_| ToolExecuteError {
                message: "sender dropped".to_string(),
            })?;

            if response == "[User Cancelled]" {
                return Ok(response);
            }

            Ok(format!(
                "User has answered your questions: {response}. You can now continue with the user's answers in mind."
            ))
        })
    }

    fn cancellable(&self) -> bool {
        false
    }
}
/// 智能解码子进程输出：中文 Windows 上 powershell.exe 重定向管道按系统 ANSI 代码页
/// （GBK）编码输出，直接按 UTF-8 解码会产生乱码。策略：
/// 1. 通过严格 UTF-8 校验 → 原样返回（纯 ASCII / UTF-8 输出零变化）；
/// 2. 否则比较 UTF-8 lossy 与 GBK 两种解码结果中的 U+FFFD 数量，取更少者（平手取 UTF-8）。
fn decode_output(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let utf8_lossy = String::from_utf8_lossy(bytes);
    let (gbk, _, _) = encoding_rs::GBK.decode(bytes);
    let utf8_errors = utf8_lossy.chars().filter(|&c| c == '\u{FFFD}').count();
    let gbk_errors = gbk.chars().filter(|&c| c == '\u{FFFD}').count();
    if gbk_errors < utf8_errors {
        gbk.into_owned()
    } else {
        utf8_lossy.into_owned()
    }
}

fn combine_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = decode_output(stdout);
    if stdout.is_empty() {
        decode_output(stderr)
    } else {
        format!("{}\n{}", stdout, decode_output(stderr))
    }
}

/// bash 中带文件路径语义的命令。
/// 这些命令的路径参数会被解析并检查是否位于工作目录之内。
const BASH_FILE_COMMANDS: &[&str] = &[
    "cat",
    "rm",
    "cp",
    "mv",
    "mkdir",
    "touch",
    "chmod",
    "chown",
    // PowerShell 别名
    "get-content",
    "set-content",
    "add-content",
    "copy-item",
    "move-item",
    "remove-item",
    "new-item",
    "rename-item",
];

/// 解析 bash 命令中指向工作目录之外的文件路径。
/// 命中则返回 external_directory 权限请求（默认询问），否则返回 None。
fn bash_external_dir_request(command: &str, work_dir: &str) -> Option<PermissionRequest> {
    let work_canonical = resolve_permission_path(work_dir, work_dir);
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let cmd = tokens.first()?.to_ascii_lowercase();
    if !BASH_FILE_COMMANDS.contains(&cmd.as_str()) {
        return None;
    }
    for arg in tokens.iter().skip(1) {
        let raw = arg.trim_matches(['"', '\'']);
        // 跳过标志位（-l）、chmod 模式（+x）、动态表达式（$var/$(...)/`...`）、glob
        if raw.starts_with('-') || raw.starts_with('+') {
            continue;
        }
        if raw.contains(['$', '(', '`']) {
            continue;
        }
        if raw.contains(['*', '?', '[']) {
            continue;
        }
        let expanded = if raw == "~" {
            dirs::home_dir().map(|h| h.display().to_string())
        } else if let Some(rest) = raw.strip_prefix("~/") {
            dirs::home_dir().map(|h| format!("{}{}", h.display(), rest))
        } else {
            Some(raw.to_string())
        };
        let Some(expanded) = expanded else { continue };
        let resolved = resolve_permission_path(&expanded, work_dir);
        if resolved.starts_with(&work_canonical) {
            continue;
        }
        let dir = if resolved.is_dir() {
            resolved
        } else {
            resolved
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or(resolved)
        };
        return Some(external_dir_request(
            &dir,
            format!(
                "Command accesses path outside working directory: {}",
                command
            ),
        ));
    }
    None
}

/// 从 bash 工具参数中提取 `command_string`。
pub(crate) fn bash_command_from_arguments(arguments: &str) -> Option<String> {
    let args: Value = serde_json::from_str(arguments).ok()?;
    args["command_string"].as_str().map(str::to_string)
}

/// plan 模式下的 bash 只读拦截：返回 `Some(拒绝原因)` 时禁止执行。
/// 只读判定 fail-closed（无法明确判定为只读即拒绝）。
pub(crate) fn plan_mode_bash_denial(arguments: &str) -> Option<String> {
    let command = bash_command_from_arguments(arguments)?;
    if crate::permission::bash_readonly::is_read_only_bash_command(&command) {
        None
    } else {
        Some(format!(
            "[Bash write operation blocked in plan mode: {command}. Plan mode is read-only; exit with /plan or Shift+Tab to execute.]"
        ))
    }
}

/// Windows shell 程序选择：优先 PowerShell 7+（pwsh.exe，管道输出默认 UTF-8），
/// 未安装时回退 powershell.exe（GBK 输出由 decode_output 兜底还原）。
/// 探测结果进程内缓存，只查 PATH 中文件是否存在、不额外起进程。
#[cfg(windows)]
fn windows_shell_program() -> &'static str {
    static SHELL: OnceLock<&'static str> = OnceLock::new();
    SHELL.get_or_init(|| {
        let has_pwsh = std::env::var_os("PATH")
            .map(|paths| {
                paths
                    .to_string_lossy()
                    .split(';')
                    .filter(|p| !p.trim().is_empty())
                    .any(|p| Path::new(p).join("pwsh.exe").is_file())
            })
            .unwrap_or(false);
        if has_pwsh {
            "pwsh.exe"
        } else {
            "powershell.exe"
        }
    })
}

pub struct BashTool;

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn permission_category(&self) -> Option<&str> {
        Some("bash")
    }

    fn extract_permission(&self, arguments: &str, work_dir: &str) -> Option<PermissionRequest> {
        let args: Value = serde_json::from_str(arguments).ok()?;
        let command = args["command_string"].as_str()?.to_string();
        if command.trim().is_empty() {
            return None;
        }
        // 指定的 workdir 在项目目录之外 → 询问
        if let Some(wd) = args["workdir"].as_str() {
            let wd_canonical = resolve_permission_path(wd, work_dir);
            let work_canonical = resolve_permission_path(work_dir, work_dir);
            if !wd_canonical.starts_with(&work_canonical) {
                return Some(external_dir_request(
                    &wd_canonical,
                    format!("Run command outside working directory: {}", wd),
                ));
            }
        }
        // 文件路径参数指向工作目录之外 → 询问
        if let Some(req) = bash_external_dir_request(&command, work_dir) {
            return Some(req);
        }
        let (pattern, description) = extract_bash_pattern(&command);
        Some(PermissionRequest {
            permission: "bash".to_string(),
            patterns: vec![pattern.clone()],
            always_patterns: vec![pattern],
            description,
        })
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
        let cmd_args = vec![
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            command,
        ];
        Box::pin(run_shell_command(
            windows_shell_program(),
            cmd_args,
            workdir,
            timeout_secs,
        ))
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
        Box::pin(run_shell_command(
            "bash",
            vec!["-c".to_string(), command],
            workdir,
            timeout_secs,
        ))
    }
}

async fn run_shell_command(
    program: &'static str,
    args: Vec<String>,
    workdir: Option<String>,
    timeout_secs: Option<u64>,
) -> Result<String, ToolExecuteError> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.kill_on_drop(true);
    cmd.args(&args);
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
    const MAX_OUTPUT_CHARS: usize = 5000;

    let raw = if output.status.success() {
        decode_output(&output.stdout)
    } else {
        combine_output(&output.stdout, &output.stderr)
    };

    let char_count = raw.chars().count();
    if char_count > MAX_OUTPUT_CHARS {
        let truncated: String = raw.chars().take(MAX_OUTPUT_CHARS).collect();
        Ok(format!(
            "{}\n\n... Output truncated (showing {} of {} characters, {} more omitted)",
            truncated,
            MAX_OUTPUT_CHARS,
            char_count,
            char_count - MAX_OUTPUT_CHARS
        ))
    } else {
        Ok(raw)
    }
}

pub struct ReadTool;

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn permission_category(&self) -> Option<&str> {
        Some("read")
    }

    fn extract_permission(&self, arguments: &str, work_dir: &str) -> Option<PermissionRequest> {
        let args: Value = serde_json::from_str(arguments).ok()?;
        let file_path = args["file_path"].as_str()?;

        let canonical = resolve_permission_path(file_path, work_dir);
        let work_canonical = resolve_permission_path(work_dir, work_dir);

        // 工作目录之外 → external_directory 询问
        if !canonical.starts_with(&work_canonical) {
            let dir = if canonical.is_dir() {
                canonical
            } else {
                canonical
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or(canonical)
            };
            return Some(external_dir_request(
                &dir,
                format!("Read outside working directory: {}", file_path),
            ));
        }

        // 工作目录内 → read 请求（默认放行；*.env 命中内置默认询问规则）
        let path_str = canonical.display().to_string();
        Some(PermissionRequest {
            permission: "read".to_string(),
            patterns: vec![path_str.clone()],
            always_patterns: vec!["*".to_string()],
            description: format!("Read file: {}", file_path),
        })
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
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
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
        })
    }
}

/// 每个变更组保留的上下文行数
const DIFF_CONTEXT_LINES: usize = 3;

/// 序列化 edit/write 的 diff 展示数据。
///
/// 只存储预计算的 hunk（变更行 + 上下文行），不存储文件全文，
/// 避免大文件多次编辑导致 runtime_meta 落库膨胀。
fn serialize_diff_data(old: Option<&str>, new: &str, file_path: &str) -> String {
    use similar::{ChangeTag, TextDiff};

    let old_lines: Vec<&str> = old.map(|s| s.lines().collect()).unwrap_or_default();
    let new_lines: Vec<&str> = new.lines().collect();

    let diff = TextDiff::from_slices(&old_lines, &new_lines);

    let mut additions = 0usize;
    let mut deletions = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            _ => {}
        }
    }

    let mut hunks = Vec::new();

    for group in diff.grouped_ops(DIFF_CONTEXT_LINES) {
        let mut old_start: Option<usize> = None;
        let mut new_start: Option<usize> = None;
        let mut changes: Vec<(char, String)> = Vec::new();

        for op in &group {
            for change in diff.iter_changes(op) {
                if old_start.is_none()
                    && let Some(i) = change.old_index()
                {
                    old_start = Some(i + 1);
                }
                if new_start.is_none()
                    && let Some(i) = change.new_index()
                {
                    new_start = Some(i + 1);
                }
                let sign = match change.tag() {
                    ChangeTag::Insert => '+',
                    ChangeTag::Delete => '-',
                    ChangeTag::Equal => ' ',
                };
                changes.push((sign, change.value().to_string()));
            }
        }

        if !changes.is_empty() {
            hunks.push(serde_json::json!({
                "old_start": old_start,
                "new_start": new_start,
                "changes": changes
                    .iter()
                    .map(|(sign, text)| serde_json::json!([sign.to_string(), text]))
                    .collect::<Vec<_>>(),
            }));
        }
    }

    serde_json::json!({
        "format": "diff",
        "path": file_path,
        "additions": additions,
        "deletions": deletions,
        "old_lines": old_lines.len(),
        "new_lines": new_lines.len(),
        "is_new_file": old.is_none(),
        "hunks": hunks,
    })
    .to_string()
}

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

    fn extract_permission(&self, arguments: &str, work_dir: &str) -> Option<PermissionRequest> {
        let args: Value = serde_json::from_str(arguments).ok()?;
        let file_path = args["file_path"].as_str()?;

        let canonical = resolve_permission_path(file_path, work_dir);
        let work_canonical = resolve_permission_path(work_dir, work_dir);

        // 工作目录之外 → external_directory 询问
        if !canonical.starts_with(&work_canonical) {
            let dir = canonical
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or(canonical);
            return Some(external_dir_request(
                &dir,
                format!("Edit outside working directory: {}", file_path),
            ));
        }

        // 工作目录内 → edit 请求（默认放行，配置规则仍可 deny）
        let path_str = canonical.display().to_string();
        Some(PermissionRequest {
            permission: "edit".to_string(),
            patterns: vec![path_str.clone()],
            always_patterns: vec!["*".to_string()],
            description: format!("Edit {}", file_path),
        })
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
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        Box::pin(async move {
            let (content, _display) = self.execute_async_with_display(arguments).await?;
            Ok(content)
        })
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

    fn allowed_in_plan_mode(&self) -> bool {
        false
    }

    fn permission_category(&self) -> Option<&str> {
        Some("write")
    }

    fn extract_permission(&self, arguments: &str, work_dir: &str) -> Option<PermissionRequest> {
        let args: Value = serde_json::from_str(arguments).ok()?;
        let file_path = args["file_path"].as_str()?;

        let canonical = resolve_permission_path(file_path, work_dir);
        let work_canonical = resolve_permission_path(work_dir, work_dir);

        // 工作目录之外 → external_directory 询问
        if !canonical.starts_with(&work_canonical) {
            let dir = canonical
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or(canonical);
            return Some(external_dir_request(
                &dir,
                format!("Write outside working directory: {}", file_path),
            ));
        }

        // 工作目录内 → write 请求（默认放行，配置规则仍可 deny）
        let path_str = canonical.display().to_string();
        Some(PermissionRequest {
            permission: "write".to_string(),
            patterns: vec![path_str.clone()],
            always_patterns: vec!["*".to_string()],
            description: format!("Write {}", file_path),
        })
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
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        Box::pin(async move {
            let (content, _display) = self.execute_async_with_display(arguments).await?;
            Ok(content)
        })
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

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
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

    fn permission_category(&self) -> Option<&str> {
        Some("webfetch")
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

    fn permission_category(&self) -> Option<&str> {
        Some("grep")
    }

    fn extract_permission(&self, arguments: &str, work_dir: &str) -> Option<PermissionRequest> {
        let args: Value = serde_json::from_str(arguments).ok()?;
        let path = args["path"].as_str().unwrap_or(".");
        let canonical = resolve_permission_path(path, work_dir);
        let work_canonical = resolve_permission_path(work_dir, work_dir);

        // 搜索路径在工作目录之外 → external_directory 询问
        if !canonical.starts_with(&work_canonical) {
            return Some(external_dir_request(
                &canonical,
                format!("Search outside working directory: {}", path),
            ));
        }

        // 工作目录内 → grep 请求（默认放行，配置规则仍可 deny）
        let path_str = canonical.display().to_string();
        Some(PermissionRequest {
            permission: "grep".to_string(),
            patterns: vec![path_str.clone()],
            always_patterns: vec!["*".to_string()],
            description: format!("Search in: {}", path),
        })
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

    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        Box::pin(async move {
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
                        results.push_str(&format!(
                            "{}: (search error: {})\n",
                            file_path.display(),
                            e
                        ));
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
        })
    }
}

pub struct GlobTool;

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn permission_category(&self) -> Option<&str> {
        Some("glob")
    }

    fn extract_permission(&self, arguments: &str, work_dir: &str) -> Option<PermissionRequest> {
        let args: Value = serde_json::from_str(arguments).ok()?;
        let path = args["path"].as_str().unwrap_or(".");
        let canonical = resolve_permission_path(path, work_dir);
        let work_canonical = resolve_permission_path(work_dir, work_dir);

        // 搜索路径在工作目录之外 → external_directory 询问
        if !canonical.starts_with(&work_canonical) {
            return Some(external_dir_request(
                &canonical,
                format!("Search outside working directory: {}", path),
            ));
        }

        // 工作目录内 → glob 请求（默认放行，配置规则仍可 deny）
        let path_str = canonical.display().to_string();
        Some(PermissionRequest {
            permission: "glob".to_string(),
            patterns: vec![path_str.clone()],
            always_patterns: vec!["*".to_string()],
            description: format!("Search in: {}", path),
        })
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

    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        Box::pin(async move {
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

            const MAX_FILES: usize = 100;

            let mut results = String::new();
            let mut file_count = 0usize;
            let mut total_count = 0usize;
            let mut truncated = false;
            for entry in walk_builder.build() {
                let entry = entry.map_err(|e| ToolExecuteError {
                    message: e.to_string(),
                })?;
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    continue;
                }
                if glob_matcher.is_match(entry.path()) {
                    total_count += 1;
                    if file_count < MAX_FILES
                        && let Ok(canonical) = entry.path().canonicalize()
                    {
                        results.push_str(&format!("{}\n", canonical.display()));
                        file_count += 1;
                    }
                    if total_count >= MAX_FILES * 5 {
                        truncated = true;
                        break;
                    }
                }
            }

            if results.is_empty() {
                Ok("No matching files found".to_string())
            } else if truncated {
                results.push_str(&format!(
                    "\n... Too many results, truncated (showing {} of {}+ files)",
                    MAX_FILES,
                    MAX_FILES * 5
                ));
                Ok(results)
            } else if total_count > MAX_FILES {
                results.push_str(&format!(
                    "\n... Too many results, truncated (showing {} of {} files, {} more omitted)",
                    MAX_FILES,
                    total_count,
                    total_count - MAX_FILES
                ));
                Ok(results)
            } else {
                Ok(results)
            }
        })
    }
}

pub struct TodoWriteTool;

impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todo_write"
    }

    fn permission_category(&self) -> Option<&str> {
        Some("todowrite")
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

    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        Box::pin(async move {
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
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_workdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hailux_perm_test_{}", name));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn diff_data_small_change_not_truncated() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nb\nX\nd\ne\nf\ng\nh\ni\nj\n";
        let v: serde_json::Value =
            serde_json::from_str(&serialize_diff_data(Some(old), new, "src/main.rs")).unwrap();
        assert_eq!(v["format"], "diff");
        assert_eq!(v["path"], "src/main.rs");
        assert_eq!(v["additions"], 1);
        assert_eq!(v["deletions"], 1);
        assert_eq!(v["old_lines"], 10);
        assert_eq!(v["new_lines"], 10);
        assert_eq!(v["is_new_file"], false);
        let hunks = v["hunks"].as_array().unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0]["old_start"], 1);
        assert_eq!(hunks[0]["new_start"], 1);
        let changes = hunks[0]["changes"].as_array().unwrap();
        assert_eq!(changes.len(), 7); // 2 前置上下文 + 1 删 + 1 增 + 3 后置上下文
        let signs: Vec<&str> = changes.iter().map(|c| c[0].as_str().unwrap()).collect();
        assert_eq!(signs, vec![" ", " ", "-", "+", " ", " ", " "]);
    }

    #[test]
    fn diff_data_new_file() {
        let v: serde_json::Value =
            serde_json::from_str(&serialize_diff_data(None, "x\ny\n", "a.txt")).unwrap();
        assert_eq!(v["is_new_file"], true);
        assert_eq!(v["additions"], 2);
        assert_eq!(v["deletions"], 0);
        assert_eq!(v["old_lines"], 0);
        let hunks = v["hunks"].as_array().unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0]["old_start"], serde_json::Value::Null);
        assert_eq!(hunks[0]["new_start"], 1);
        let signs: Vec<&str> = hunks[0]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c[0].as_str().unwrap())
            .collect();
        assert_eq!(signs, vec!["+", "+"]);
    }

    #[test]
    fn diff_data_keeps_large_diffs_untruncated() {
        let old: String = (0..100).map(|i| format!("old{i}\n")).collect();
        let new: String = (0..100).map(|i| format!("new{i}\n")).collect();
        let v: serde_json::Value =
            serde_json::from_str(&serialize_diff_data(Some(&old), &new, "big.txt")).unwrap();
        assert_eq!(v["additions"], 100);
        assert_eq!(v["deletions"], 100);
        // 全量保留：整文件替换无相同行 → 无上下文，100 删 + 100 增
        let total: usize = v["hunks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["changes"].as_array().unwrap().len())
            .sum();
        assert_eq!(total, 200);
    }

    #[test]
    fn diff_data_identical_content_yields_no_hunks() {
        let v: serde_json::Value =
            serde_json::from_str(&serialize_diff_data(Some("a\nb\n"), "a\nb\n", "same.txt"))
                .unwrap();
        assert_eq!(v["additions"], 0);
        assert_eq!(v["deletions"], 0);
        assert_eq!(v["hunks"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn bash_inside_workdir_is_none() {
        let wd = tmp_workdir("inside");
        let wd_str = wd.display().to_string();
        // 普通命令 / 目录内文件 / 目录内写操作：均无需权限
        assert!(bash_external_dir_request("ls -la", &wd_str).is_none());
        assert!(bash_external_dir_request("git status", &wd_str).is_none());
        assert!(bash_external_dir_request("cat src/main.rs", &wd_str).is_none());
        assert!(bash_external_dir_request("rm -rf ./target", &wd_str).is_none());
        assert!(bash_external_dir_request("mkdir -p build", &wd_str).is_none());
    }

    #[test]
    fn bash_outside_workdir_requests_external_directory() {
        let wd = tmp_workdir("outside");
        let wd_str = wd.display().to_string();
        let req = bash_external_dir_request("cat ../secrets.txt", &wd_str);
        let req = req.expect("external path must be gated");
        assert_eq!(req.permission, "external_directory");
        // 绝对路径外部文件
        assert!(bash_external_dir_request("cat /etc/hosts", &wd_str).is_some());
        // 非文件命令不受文件路径检查影响（默认放行，走 bash 规则）
        assert!(bash_external_dir_request("npm install", &wd_str).is_none());
        assert!(bash_external_dir_request("curl https://example.com", &wd_str).is_none());
    }

    #[test]
    fn bash_skips_flags_modes_and_dynamic_args() {
        let wd = tmp_workdir("flags");
        let wd_str = wd.display().to_string();
        // -l 标志、chmod +x 模式、$HOME 动态展开均跳过
        assert!(bash_external_dir_request("cat -n /etc/hosts", &wd_str).is_some());
        // ls 不参与文件路径检查
        assert!(bash_external_dir_request("ls -l /etc", &wd_str).is_none());
        assert!(bash_external_dir_request("chmod +x script.sh", &wd_str).is_none());
        assert!(bash_external_dir_request("cat $HOME/.ssh/id_rsa", &wd_str).is_none());
        assert!(bash_external_dir_request("cat ~/.ssh/id_rsa", &wd_str).is_some());
    }

    #[test]
    fn normalize_lexical_resolves_parent_dirs() {
        let base = tmp_workdir("norm");
        let joined = Path::new(&base).join("../outside_dir");
        assert_eq!(
            normalize_lexical(&joined),
            base.parent().unwrap().join("outside_dir")
        );
    }

    #[test]
    fn plan_mode_denies_bash_write_commands() {
        let deny = |args: &str| plan_mode_bash_denial(args);
        // 写操作 → 拒绝
        assert!(deny(r#"{"command_string":"rm -rf target"}"#).is_some());
        assert!(deny(r#"{"command_string":"git commit -m x"}"#).is_some());
        assert!(deny(r#"{"command_string":"echo hi > f"}"#).is_some());
        // 只读操作 → 放行
        assert!(deny(r#"{"command_string":"git status"}"#).is_none());
        assert!(deny(r#"{"command_string":"git diff HEAD"}"#).is_none());
        assert!(deny(r#"{"command_string":"ls -la"}"#).is_none());
        // 坏 JSON / 缺失参数 → 放行（由执行层报错）
        assert!(deny("not json").is_none());
        assert!(deny(r#"{}"#).is_none());
        // 拒绝信息包含命令原文，便于模型理解
        let reason = deny(r#"{"command_string":"rm -rf target"}"#).unwrap();
        assert!(reason.contains("rm -rf target"));
        assert!(reason.contains("plan mode"));
    }

    #[test]
    fn decode_output_passthrough_utf8() {
        // 纯 ASCII 与合法 UTF-8（含中文）原样返回
        assert_eq!(decode_output(b"hello world"), "hello world");
        let text = "中文输出 cargo build";
        assert_eq!(decode_output(text.as_bytes()), text);
    }

    #[test]
    fn decode_output_restores_gbk_text() {
        // powershell.exe 在中文 Windows 上按 GBK 输出错误记录，应正确还原
        let text = "拒绝访问。 (os error 5)\n所在位置 行:1 字符: 247";
        let (bytes, _, _) = encoding_rs::GBK.encode(text);
        assert_eq!(decode_output(&bytes), text);
    }

    #[test]
    fn decode_output_prefers_utf8_on_error_tie() {
        // UTF-8 主体 + 少量坏字节：错误数打平（或 UTF-8 更少）时不应整体转 GBK
        let mut bytes = "中文输出 cargo build".as_bytes().to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE]);
        let decoded = decode_output(&bytes);
        assert!(decoded.starts_with("中文输出 cargo build"));
        assert!(decoded.contains('\u{FFFD}'));
    }

    #[cfg(windows)]
    #[test]
    fn windows_shell_program_resolves() {
        // PATH 探测不 panic，且返回两个候选之一
        let shell = windows_shell_program();
        assert!(shell == "pwsh.exe" || shell == "powershell.exe");
    }
}
