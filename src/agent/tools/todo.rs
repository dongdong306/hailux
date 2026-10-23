use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};

use super::{Tool, ToolExecuteError};

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
        _work_dir: &'a str,
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
