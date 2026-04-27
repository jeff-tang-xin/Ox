use serde_json::{json, Value};

use super::{SafetyLevel, Tool, ToolContext, ToolOutput};

pub struct WebFetchTool;

#[async_trait::async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch the content of a URL. Returns the response body as text (truncated to 10000 chars)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                }
            },
            "required": ["url"]
        })
    }

    fn safety_level(&self) -> SafetyLevel {
        SafetyLevel::Safe
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let url = match args.get("url").and_then(|u| u.as_str()) {
            Some(u) => u,
            None => return ToolOutput::error("Missing required parameter: url. Usage: {\"url\": \"<url>\"}"),
        };

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
                        let truncated = if body.len() > 10000 {
                            let end = body.char_indices().take_while(|(i, _)| *i < 10000).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
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
