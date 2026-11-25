use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};
use tokio::sync::oneshot;

use super::{Tool, ToolExecuteError};
use crate::agent::event::{CoreEvent, QuestionInfo, QuestionOption};

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
        _work_dir: &'a str,
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
