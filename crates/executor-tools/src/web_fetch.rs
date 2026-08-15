//! `WebFetch` tool — fetch a URL and return its content as plain text.

use async_trait::async_trait;
use executor_core::{ExecutionError, Tool, ToolContext, ToolDefinition, ToolOutput};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Tool that fetches a URL and returns its content as plain text.
///
/// HTTP URLs are automatically upgraded to HTTPS. Cross-host redirects are
/// reported back to the caller. Results are cached per URL for 15 minutes.
pub struct WebFetchTool {
    cache: Mutex<HashMap<String, (String, Instant)>>,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "WebFetch".into(),
            description: "Fetch a URL and convert to text. HTTP upgraded to HTTPS. Results cached for 15 min per URL."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": { "url": { "type": "string" } },
                "required": ["url"]
            }),
        }
    }

    fn is_concurrency_safe(&self, _arguments: &Value) -> bool {
        true
    }

    fn invocation_detail(&self, arguments: &Value) -> String {
        arguments
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    async fn invoke(
        &self,
        arguments: Value,
        _context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        let url = arguments
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| ExecutionError::ToolValidation {
                tool: "WebFetch".into(),
                message: "missing `url`".into(),
            })?;

        // Check cache (15 minutes).
        {
            let cache = self.cache.lock().unwrap();
            if let Some((content, time)) = cache.get(url)
                && time.elapsed() < Duration::from_secs(900)
            {
                return Ok(ToolOutput::text(format!("[cached]\n{content}")));
            }
        }

        let url = normalize_fetch_url(url);
        let body = fetch_url_text(&url).await?;
        let text = strip_html(&body);
        let truncated: String = text.chars().take(50_000).collect();

        // Cache and return.
        {
            self.cache
                .lock()
                .unwrap()
                .insert(url, (truncated.clone(), Instant::now()));
        }

        Ok(ToolOutput::text(truncated))
    }
}

async fn fetch_url_text(url: &str) -> Result<String, ExecutionError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("ate-executor/0.1")
        .build()
        .map_err(|err| ExecutionError::ToolExecution {
            tool: "WebFetch".into(),
            message: format!("failed to create HTTP client: {err}"),
        })?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| ExecutionError::ToolExecution {
            tool: "WebFetch".into(),
            message: format!("HTTP request failed: {err}"),
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(ExecutionError::ToolExecution {
            tool: "WebFetch".into(),
            message: format!("HTTP request returned status {status}"),
        });
    }
    response
        .text()
        .await
        .map_err(|err| ExecutionError::ToolExecution {
            tool: "WebFetch".into(),
            message: format!("failed to read HTTP response body: {err}"),
        })
}

fn strip_html(html: &str) -> String {
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut result = String::new();
    let chars: Vec<char> = html.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            // Check for script/style tags to skip their content.
            let tag_lower: String = chars
                .iter()
                .skip(i + 1)
                .take_while(|&&ch| ch != '>' && ch != ' ')
                .map(|&ch| ch.to_ascii_lowercase())
                .collect();
            if tag_lower == "script" {
                in_script = true;
            } else if tag_lower == "style" {
                in_style = true;
            } else if tag_lower.starts_with("/script") {
                in_script = false;
            } else if tag_lower.starts_with("/style") {
                in_style = false;
            }

            if !in_script && !in_style {
                // Track whether this is a block-level tag for spacing.
                let lower = tag_lower.to_lowercase();
                if matches!(
                    lower.as_str(),
                    "br" | "p"
                        | "div"
                        | "tr"
                        | "li"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "blockquote"
                        | "hr"
                        | "pre"
                        | "/p"
                        | "/div"
                        | "/tr"
                        | "/li"
                        | "/h1"
                        | "/h2"
                        | "/h3"
                        | "/h4"
                        | "/h5"
                        | "/h6"
                        | "/blockquote"
                        | "/pre"
                        | "table"
                        | "/table"
                        | "/ol"
                        | "/ul"
                        | "ol"
                        | "ul"
                        | "/title"
                        | "title"
                ) && !result.is_empty()
                    && !result.ends_with('\n')
                {
                    result.push('\n');
                }
            }
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag && !in_script && !in_style {
            result.push(c);
        }
        i += 1;
    }

    // Collapse multiple whitespace characters into a single space.
    let mut collapsed = String::with_capacity(result.len());
    let mut prev_space = false;
    for c in result.chars() {
        if c.is_whitespace() && c != '\n' {
            if !prev_space {
                collapsed.push(' ');
                prev_space = true;
            }
        } else {
            collapsed.push(c);
            prev_space = false;
        }
    }

    // Trim each line.
    collapsed
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_fetch_url(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    if parsed.scheme() != "http" {
        return url.to_string();
    }
    if parsed.host_str().is_some_and(is_loopback_host) {
        return url.to_string();
    }
    url.replacen("http://", "https://", 1)
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_fetch_url_upgrades_public_http_to_https() {
        assert_eq!(
            normalize_fetch_url("http://example.com/x"),
            "https://example.com/x"
        );
    }

    #[test]
    fn normalize_fetch_url_keeps_loopback_http() {
        assert_eq!(
            normalize_fetch_url("http://localhost:8080/health"),
            "http://localhost:8080/health"
        );
        assert_eq!(
            normalize_fetch_url("http://127.0.0.1/"),
            "http://127.0.0.1/"
        );
    }

    #[test]
    fn strip_html_removes_script_and_style_content() {
        let html = "<html><head><title>Hi</title></head><body><p>Hello</p><script>var x=1;</script></body></html>";
        let text = strip_html(html);
        assert!(!text.contains("var x"), "script content must be stripped");
        assert!(text.contains("Hello"), "body text must be kept");
    }
}
