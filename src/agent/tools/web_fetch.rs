use std::future::Future;
use std::pin::Pin;

use mdka::html_to_markdown;
use reqwest::Client;
use serde_json::{Value, json};

use super::{Tool, ToolExecuteError};

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
        _work_dir: &'a str,
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
