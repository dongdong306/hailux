use async_openai::types::chat::{ChatCompletionTool, ChatCompletionTools, FunctionObject};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use crate::agent::media::Attachment;
use crate::permission::PermissionRequest;
use crate::permission::resolve_permission_path;

mod ask;
mod bash;
mod diff;
mod edit;
mod glob;
mod grep;
mod read;
mod todo;
mod web_fetch;
mod write;

#[cfg(test)]
mod tests;

pub use ask::AskTool;
pub use bash::BashTool;
pub(crate) use bash::plan_mode_bash_denial;
#[cfg(windows)]
pub use bash::prewarm_shell_description;
pub use edit::EditTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use todo::TodoWriteTool;
pub use web_fetch::WebFetchTool;
pub use write::WriteTool;

/// 工具执行结果：文本输出 + 可选 UI 展示数据 + 可选媒体附件。
///
/// 附件约定：文本输出只放占位说明文字，
/// 媒体本体（`data:<mime>;base64,<data>`）放 attachments，
/// 由 agent loop 在请求组装层决定内联或降级。
#[derive(Debug, Clone, Default)]
pub struct ToolExecution {
    pub content: String,
    pub display: Option<String>,
    pub attachments: Vec<Attachment>,
}

impl ToolExecution {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            display: None,
            attachments: Vec::new(),
        }
    }
}

type ToolFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ToolExecution, ToolExecuteError>> + Send + 'a>>;

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

/// 构造「操作非工作目录内容」的权限请求（external_directory，默认询问）。
fn external_dir_request(dirs: &[std::path::PathBuf], description: String) -> PermissionRequest {
    let patterns: Vec<String> = dirs
        .iter()
        .map(|d| d.join("*").display().to_string())
        .collect();
    PermissionRequest {
        permission: "external_directory".to_string(),
        patterns: patterns.clone(),
        always_patterns: patterns,
        description,
        force_ask: false,
    }
}

/// 若路径解析在工作目录之外，返回应询问的目录（文件取父目录，目录取自身）。
/// 工作目录之内返回 None。
fn external_dir_of(path: &str, work_dir: &str) -> Option<std::path::PathBuf> {
    let canonical = resolve_permission_path(path, work_dir);
    let work_canonical = resolve_permission_path(work_dir, work_dir);
    if canonical.starts_with(&work_canonical) {
        return None;
    }
    Some(if canonical.is_dir() {
        canonical
    } else {
        canonical
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or(canonical)
    })
}

/// 从工具参数 JSON 中取字符串字段；解析失败或字段缺失返回 None。
fn argument_str(arguments: &str, key: &str) -> Option<String> {
    let args: Value = serde_json::from_str(arguments).ok()?;
    args[key].as_str().map(str::to_string)
}

/// 工具执行路径解析：绝对路径原样返回；相对路径按会话工作目录拼接。
/// 纯词法拼接（不 canonicalize），write/edit 的目标文件尚不存在时也能解析。
/// 与权限检查 `resolve_permission_path` 同基准——修复 Web 模式下选中工作
/// 目录 ≠ 服务进程启动目录时，`.` 与相对路径错误落到启动目录的问题。
fn resolve_tool_path(path: &str, work_dir: &str) -> String {
    if is_absoluteish(path) {
        return path.to_string();
    }
    // components() 归一化：去掉 `.` 组件与重复分隔符，统一平台分隔符
    Path::new(work_dir)
        .join(path)
        .components()
        .collect::<std::path::PathBuf>()
        .to_string_lossy()
        .to_string()
}

/// 视为"不按 work_dir 拼接"的路径：常规绝对路径；Windows 下另含盘符相对
/// 路径（如 `D:foo`，指向 D 盘当前目录，拼接 work_dir 反而错误）。
#[cfg(windows)]
fn is_absoluteish(path: &str) -> bool {
    Path::new(path).is_absolute() || (path.len() >= 2 && path.as_bytes()[1] == b':')
}

#[cfg(not(windows))]
fn is_absoluteish(path: &str) -> bool {
    Path::new(path).is_absolute()
}

/// 展示用规范化路径：canonicalize 后剥离 Windows `\\?\` verbatim 前缀。
fn display_canonical(path: &Path) -> String {
    match path.canonicalize() {
        Ok(c) => {
            let s = c.to_string_lossy().to_string();
            s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
        }
        Err(_) => path.to_string_lossy().to_string(),
    }
}

/// 工具 trait，所有可调用的工具都需要实现它
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;

    /// 异步执行工具。所有工具都必须实现真正的异步执行，
    /// 避免在 executor 线程上做阻塞 I/O 或绕过 timeout。
    ///
    /// `work_dir` 为当前会话的工作目录（由 agent 派发层注入，与
    /// `extract_permission` 同源）：工具内的相对路径参数一律按它解析，
    /// 不得依赖进程 cwd —— Web 模式下两者并不一致。
    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
        work_dir: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>>;

    /// 异步执行并返回用于 UI 展示的额外数据（如 diff）与媒体附件。
    /// 默认委托给 `execute_async`，display 返回 `None`、attachments 为空。
    /// 需要提供 UI 专用展示信息的工具（如 edit/write）或媒体附件的工具（如 read）覆写此方法。
    fn execute_async_with_display<'a>(
        &'a self,
        arguments: &'a str,
        work_dir: &'a str,
    ) -> ToolFuture<'a> {
        let fut = self.execute_async(arguments, work_dir);
        Box::pin(async move {
            let content = fut.await?;
            Ok(ToolExecution {
                content,
                display: None,
                attachments: Vec::new(),
            })
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
    ///
    /// 返回有序请求列表：调用方依序评估，任一 Deny 短路拒绝，第一个 Ask
    /// 弹窗，全部 Allow 才放行。空列表表示不需要权限检查（即使
    /// permission_category 返回了 Some）。
    ///
    /// 多请求场景：一条 bash 命令可同时命中 bash 命令规则（如
    /// `[permission.bash] "rm *" = "deny"`）与越界路径检查，列表中 bash
    /// 请求在前、external_directory 请求在后——高优先级类别先评估，
    /// deny 规则不会被越界请求短路。
    fn extract_permission(&self, _arguments: &str, _work_dir: &str) -> Vec<PermissionRequest> {
        Vec::new()
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
