use serde_json::{Value, json};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct WebFetchTool;

#[async_trait::async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch URL content. Returns response body as text (truncated to 10000 chars)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "✅ REQUIRED: URL to fetch (must include http:// or https://)"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Max characters to return. Default 10000. Increase for large API responses.",
                    "minimum": 100,
                    "maximum": 100000
                }
            },
            "required": ["url"],
            "examples": [
                {"url": "https://example.com/api/docs"},
                {"url": "https://raw.githubusercontent.com/user/repo/main/README.md"}
            ]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let url = match args.get("url").and_then(|u| u.as_str()) {
            Some(u) => u,
            None => {
                return ToolOutput::error(
                    "Missing required parameter: url. Usage: {\"url\": \"<url>\"}",
                );
            }
        };
        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(10000) as usize;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build();

        let client = match client {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to create HTTP client: {e}")),
        };

        match client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                match resp.text().await {
                    Ok(body) => {
                        let truncated = if body.len() > max_chars {
                            let end = body
                                .char_indices()
                                .take_while(|(i, _)| *i < max_chars)
                                .last()
                                .map(|(i, c)| i + c.len_utf8())
                                .unwrap_or(0);
                            format!(
                                "{}\n\n... (truncated, {} total chars)",
                                &body[..end],
                                body.len()
                            )
                        } else {
                            body
                        };
                        if status.is_success() {
                            ToolOutput::success(truncated)
                        } else {
                            ToolOutput::error(format!("HTTP {status}:\n{truncated}"))
                        }
                    }
                    Err(e) => ToolOutput::error(format!("Failed to read response body: {e}")),
                }
            }
            Err(e) => ToolOutput::error(format!("Failed to fetch {url}: {e}")),
        }
    }
}
