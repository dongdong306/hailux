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

mod diff;
mod edit;
mod glob;
mod grep;
mod question;
mod read;
mod shell;
mod todo;
mod web_fetch;
mod write;

#[cfg(test)]
mod tests;

pub use edit::EditTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use question::QuestionTool;
pub use read::ReadTool;
pub use shell::ShellTool;
pub(crate) use shell::plan_mode_shell_denial;
#[cfg(windows)]
pub use shell::prewarm_shell_description;
pub use todo::TodoWriteTool;
pub use web_fetch::WebFetchTool;
pub use write::WriteTool;

/// Tool execution result: text output + optional UI display data + optional
/// media attachments.
///
/// Attachment convention: the text output only carries a placeholder note,
/// the media itself (`data:<mime>;base64,<data>`) goes into attachments,
/// and the agent loop decides at the request-assembly layer whether to
/// inline it or degrade.
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

/// Builds the permission request for operating on content outside the working
/// directory (external_directory; asks by default).
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

/// Returns the directory to ask about when the resolved path lies outside the
/// working directory (parent directory for files, the directory itself for
/// directories). Returns `None` when the path is inside the working directory.
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

/// Reads a string field from the tool-arguments JSON; returns `None` on parse
/// failure or when the field is missing.
fn argument_str(arguments: &str, key: &str) -> Option<String> {
    let args: Value = serde_json::from_str(arguments).ok()?;
    args[key].as_str().map(str::to_string)
}

/// Tool execution path resolution: absolute paths are returned as-is; relative
/// paths are joined against the session working directory. Purely lexical join
/// (no canonicalize), so write/edit targets that do not exist yet resolve too.
/// Uses the same base as the permission check `resolve_permission_path` — fixes
/// the Web-mode issue where, when the selected working directory ≠ the service
/// process startup directory, `.` and relative paths wrongly landed in the
/// startup directory.
fn resolve_tool_path(path: &str, work_dir: &str) -> String {
    if is_absoluteish(path) {
        return path.to_string();
    }
    // components() normalization: strips `.` components and duplicate
    // separators, unifying the platform separator
    Path::new(work_dir)
        .join(path)
        .components()
        .collect::<std::path::PathBuf>()
        .to_string_lossy()
        .to_string()
}

/// Paths treated as "not joined against work_dir": regular absolute paths; on
/// Windows this also covers drive-relative paths (e.g. `D:foo`, which points at
/// the current directory of drive D — joining work_dir would be wrong).
#[cfg(windows)]
fn is_absoluteish(path: &str) -> bool {
    Path::new(path).is_absolute() || (path.len() >= 2 && path.as_bytes()[1] == b':')
}

#[cfg(not(windows))]
fn is_absoluteish(path: &str) -> bool {
    Path::new(path).is_absolute()
}

/// Normalized path for display: canonicalizes then strips the Windows `\\?\` verbatim prefix.
fn display_canonical(path: &Path) -> String {
    match path.canonicalize() {
        Ok(c) => {
            let s = c.to_string_lossy().to_string();
            s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
        }
        Err(_) => path.to_string_lossy().to_string(),
    }
}

/// Tool trait; every callable tool must implement it
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;

    /// Executes the tool asynchronously. Every tool must implement true async
    /// execution, avoiding blocking I/O or timeout bypass on the executor thread.
    ///
    /// `work_dir` is the current session's working directory (injected by the
    /// agent dispatch layer, same source as `extract_permission`): relative
    /// path arguments inside a tool must always be resolved against it and
    /// must not rely on the process cwd — the two differ in Web mode.
    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
        work_dir: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>>;

    /// Executes asynchronously and additionally returns data for UI display
    /// (e.g. a diff) and media attachments. By default it delegates to
    /// `execute_async`, with `display` set to `None` and empty attachments.
    /// Tools that provide UI-specific display info (e.g. edit/write) or media
    /// attachments (e.g. read) override this method.
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

    /// Whether this tool can be interrupted by the cancel flag. Tools that
    /// block on user input (e.g. question) override this to `false` so an
    /// interrupt cannot leave a dangling dialog or a closed channel behind.
    fn cancellable(&self) -> bool {
        true
    }

    /// Returns this tool's permission category (e.g. "shell", "read", "edit",
    /// "write", "mcp"). Returns `None` when the tool needs no permission check.
    fn permission_category(&self) -> Option<&str> {
        None
    }

    /// Extracts the information needed for permission checks from tool arguments.
    ///
    /// Returns an ordered request list: the caller evaluates it in order — any
    /// Deny short-circuits, the first Ask opens a dialog, all Allow approves.
    /// An empty list means no permission check is needed (even if
    /// permission_category returned Some).
    ///
    /// Multi-request case: one shell command can hit both the shell command
    /// rule (e.g. `[permission.shell] "rm *" = "deny"`) and the out-of-workdir
    /// path check; the shell request comes first, external_directory second —
    /// the higher-priority category is evaluated first, so deny rules are not
    /// short-circuited by the out-of-workdir request.
    fn extract_permission(&self, _arguments: &str, _work_dir: &str) -> Vec<PermissionRequest> {
        Vec::new()
    }
}

/// Tool registry managing all registered tools
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

    /// Converts to the tools format required by the OpenAI API.
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

    /// Clones the registry (shared via Arc reference counting)
    pub fn clone_registry(&self) -> ToolRegistry {
        ToolRegistry {
            tools: self.tools.clone(),
        }
    }
}
