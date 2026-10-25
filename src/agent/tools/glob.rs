use std::future::Future;
use std::pin::Pin;

use ignore::WalkBuilder;
use serde_json::{Value, json};

use super::{
    Tool, ToolExecuteError, argument_str, display_canonical, external_dir_of, external_dir_request,
    resolve_tool_path,
};
use crate::agent::utils::compare_mtime;
use crate::permission::PermissionRequest;
use crate::permission::resolve_permission_path;

pub struct GlobTool;

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn permission_category(&self) -> Option<&str> {
        Some("glob")
    }

    fn extract_permission(&self, arguments: &str, work_dir: &str) -> Vec<PermissionRequest> {
        let path = argument_str(arguments, "path").unwrap_or_else(|| ".".to_string());
        let canonical = resolve_permission_path(&path, work_dir);

        // 搜索路径在工作目录之外 → external_directory 询问
        if let Some(dir) = external_dir_of(&path, work_dir) {
            return vec![external_dir_request(
                &[dir],
                format!("Search outside working directory: {}", path),
            )];
        }

        // 工作目录内 → glob 请求（默认放行，配置规则仍可 deny）
        vec![PermissionRequest {
            permission: "glob".to_string(),
            patterns: vec![canonical.display().to_string()],
            always_patterns: vec!["*".to_string()],
            description: format!("Search in: {}", path),
            force_ask: false,
        }]
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
        work_dir: &'a str,
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
            let path = args["path"]
                .as_str()
                .map_or_else(|| work_dir.to_string(), |p| resolve_tool_path(p, work_dir));

            // 与 grep 同理：目录遍历是阻塞 I/O，放 blocking 线程池执行
            let task = tokio::task::spawn_blocking(move || -> Result<String, ToolExecuteError> {
                let mut walk_builder = WalkBuilder::new(&path);
                walk_builder.sort_by_file_path(compare_mtime);
                walk_builder.hidden(false);
                walk_builder.filter_entry(|entry| {
                    !crate::agent::IGNORED_DIRS
                        .contains(&entry.file_name().to_string_lossy().as_ref())
                });

                let glob_matcher = globset::Glob::new(&pattern)
                    .map_err(|err| ToolExecuteError {
                        message: format!("Invalid glob pattern: {}", err),
                    })?
                    .compile_matcher();

                const MAX_FILES: usize = 100;

                // 双路匹配：pattern 既可匹配完整路径（绝对 pattern、`*.rs` 等
                // 头部通配形态），也可匹配相对于遍历根的路径（`src/tui/*.rs`
                // 等带相对目录前缀的形态——单匹配绝对路径时永远吃不掉前缀）。
                let root = std::path::PathBuf::from(&path);

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
                    let matched = glob_matcher.is_match(entry.path())
                        || entry
                            .path()
                            .strip_prefix(&root)
                            .map(|rel| glob_matcher.is_match(rel))
                            .unwrap_or(false);
                    if matched {
                        total_count += 1;
                        if file_count < MAX_FILES {
                            results.push_str(&format!("{}\n", display_canonical(entry.path())));
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
            });

            match task.await {
                Ok(result) => result,
                Err(err) => Err(ToolExecuteError {
                    message: format!("glob background task failed: {err}"),
                }),
            }
        })
    }
}
