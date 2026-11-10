//! Optional Haiku helper: writes a short, non-executable tag line per tick
//! for the journal.
//!
//! The helper runs in a background task after the decision, holds only the
//! shared Messages API caller (no broker client, no broker secrets, no risk
//! limits), offers no tools, and its text is treated as untrusted data: it is
//! sanitized, journaled, and never parsed as a `Decision` or fed back into
//! the decision prompt.

use super::budget::Role;
use super::llm::{sanitize_text, CallRequest, LlmCaller};
use super::LlmCallReport;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::Instant;

const MAX_NOTE_CHARS: usize = 160;

pub const HELPER_SYSTEM_PROMPT: &str = "You label market ticks for an audit log. Reply with up to 12 comma-separated lowercase tags describing the regime (for example trend, volatility, spread, position). Output only the tags.";

#[derive(Debug, Clone)]
pub struct HaikuHelper {
    model: String,
    max_output_tokens: u32,
    timeout: Duration,
}

impl HaikuHelper {
    pub fn new(model: &str, max_output_tokens: u32, timeout: Duration) -> Self {
        Self {
            model: model.to_string(),
            max_output_tokens,
            timeout,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn request_body(&self, state_json: &str, rows: &[String]) -> Value {
        json!({
            "model": self.model,
            "max_tokens": self.max_output_tokens,
            "system": HELPER_SYSTEM_PROMPT,
            "messages": [{
                "role": "user",
                "content": format!("State:\n{state_json}\n\nRecent rows:\n{}", rows.join("\n"))
            }],
            "output_config": {"effort": "medium"}
        })
    }

    pub fn spawn(&self, caller: Arc<LlmCaller>, body: Value) -> JoinHandle<LlmCallReport> {
        let model = self.model.clone();
        let max_output_tokens = self.max_output_tokens;
        let timeout = self.timeout;
        tokio::spawn(async move {
            let (mut report, result) = caller
                .call(CallRequest {
                    role: Role::Helper,
                    model: &model,
                    body,
                    max_output_tokens,
                    deadline: Instant::now() + timeout,
                })
                .await;
            if let Ok(v) = result {
                report.note = Some(helper_note(&v));
            }
            report
        })
    }
}

/// Text blocks only, sanitized and capped. Tool calls or JSON in the text are
/// just characters here.
pub fn helper_note(v: &Value) -> String {
    let text: Vec<&str> = v
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    sanitize_text(&text.join(" "), MAX_NOTE_CHARS)
}
