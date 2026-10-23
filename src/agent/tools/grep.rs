use std::future::Future;
use std::pin::Pin;

use grep::regex::RegexMatcher;
use grep::searcher::Searcher;
use grep::searcher::sinks::UTF8;
use ignore::WalkBuilder;
use serde_json::{Value, json};

use super::{
    Tool, ToolExecuteError, argument_str, display_canonical, external_dir_of, external_dir_request,
    resolve_tool_path,
};
use crate::agent::utils::compare_mtime;
use crate::permission::PermissionRequest;
use crate::permission::resolve_permission_path;

pub struct GrepTool;

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn permission_category(&self) -> Option<&str> {
        Some("grep")
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

        // 工作目录内 → grep 请求（默认放行，配置规则仍可 deny）
        vec![PermissionRequest {
            permission: "grep".to_string(),
            patterns: vec![canonical.display().to_string()],
            always_patterns: vec!["*".to_string()],
            description: format!("Search in: {}", path),
            force_ask: false,
        }]
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
        work_dir: &'a str,
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
            let path = args["path"]
                .as_str()
                .map_or_else(|| work_dir.to_string(), |p| resolve_tool_path(p, work_dir));
            let include = args["include"].as_str().map(str::to_string);

            // 遍历 + 文件扫描是纯阻塞 I/O，放 blocking 线程池执行：不占用 reactor
            // worker，且 agent.rs 的 cancel select 分支能在下方 await 挂起期间被 poll
            let task = tokio::task::spawn_blocking(move || -> Result<String, ToolExecuteError> {
                let matcher = RegexMatcher::new(&pattern).map_err(|err| ToolExecuteError {
                    message: format!("Invalid regex: {}", err),
                })?;

                let mut walk_builder = WalkBuilder::new(&path);
                walk_builder.sort_by_file_path(compare_mtime);
                walk_builder.hidden(false);
                walk_builder.filter_entry(|entry| {
                    !crate::agent::IGNORED_DIRS
                        .contains(&entry.file_name().to_string_lossy().as_ref())
                });
                if let Some(glob) = &include {
                    let mut override_builder = ignore::overrides::OverrideBuilder::new(&path);
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
                    // sink 返回 false 只能停当前文件，这里终止整树遍历
                    if match_count >= MAX_MATCHES {
                        break;
                    }
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
                            file_lines.push(format!("  {}: {}\n", lnum, line.trim_end()));
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
                        results.push_str(&format!("{}:\n", display_canonical(&file_path)));
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
            });

            match task.await {
                Ok(result) => result,
                Err(err) => Err(ToolExecuteError {
                    message: format!("grep background task failed: {err}"),
                }),
            }
        })
    }
}
