//! Bounded Messages API calls shared by the decision and helper roles.
//!
//! Every attempt (retries included) reserves its worst-case charge in the
//! [`BudgetLedger`] before dispatch and settles against the response `usage`.
//! The API key only ever goes into the `x-api-key` header.

use super::budget::{BudgetError, Clock, Role, SharedLedger};
use super::{DeciderError, LlmCallReport};
use crate::live::config::{LlmConfig, ModelPrice, MAX_PROMPT_TOKENS};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;
use tokio::time::Instant;

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Allowance for tokens the API adds around the request body (tool-use
/// system prompt, message framing). Request bytes bound the rest: a token
/// is never shorter than one byte of JSON.
pub const REQUEST_OVERHEAD_TOKENS: u64 = 1_024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

impl Usage {
    /// `None` when `input_tokens` / `output_tokens` are missing or malformed.
    pub fn from_response(v: &Value) -> Option<Self> {
        let u = v.get("usage")?;
        let opt = |k: &str| match u.get(k) {
            None | Some(Value::Null) => Some(0),
            Some(x) => x.as_u64(),
        };
        Some(Self {
            input_tokens: u.get("input_tokens")?.as_u64()?,
            output_tokens: u.get("output_tokens")?.as_u64()?,
            cache_read_input_tokens: opt("cache_read_input_tokens")?,
            cache_creation_input_tokens: opt("cache_creation_input_tokens")?,
        })
    }

    pub fn prompt_tokens(&self) -> u64 {
        self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens
    }

    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read_input_tokens += o.cache_read_input_tokens;
        self.cache_creation_input_tokens += o.cache_creation_input_tokens;
    }

    pub fn cost_usd(&self, p: &ModelPrice) -> f64 {
        p.cost_usd(
            self.input_tokens,
            self.output_tokens,
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CallLimits {
    pub max_input_tokens: u64,
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
    pub max_retries: u32,
    pub retry_backoff: Duration,
}

impl CallLimits {
    pub fn from_config(cfg: &LlmConfig) -> Self {
        Self {
            max_input_tokens: cfg.max_input_tokens,
            request_timeout: Duration::from_secs(cfg.request_timeout_secs),
            max_response_bytes: cfg.max_response_bytes,
            max_retries: cfg.max_retries,
            retry_backoff: Duration::from_millis(cfg.retry_backoff_ms),
        }
    }
}

pub struct CallRequest<'a> {
    pub role: Role,
    pub model: &'a str,
    pub body: Value,
    /// Must equal `body.max_tokens`; bounds the reservation.
    pub max_output_tokens: u32,
    /// Hard deadline; no attempt starts or runs past it.
    pub deadline: Instant,
}

enum Attempt {
    Status(u16, Vec<u8>),
    /// Failed before the request reached the API: definitely not charged.
    NotSent(String),
    Timeout,
    /// Failed after sending: outcome unknown.
    Failed(String),
}

pub struct LlmCaller {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
    pricing: BTreeMap<String, ModelPrice>,
    limits: CallLimits,
    ledger: SharedLedger,
    clock: Clock,
}

impl fmt::Debug for LlmCaller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmCaller")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .field("limits", &self.limits)
            .finish()
    }
}

fn budget(e: BudgetError) -> DeciderError {
    DeciderError::Budget(e)
}

/// `error.type` from an API error body, never the full body.
fn api_error_type(bytes: &[u8]) -> String {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|v| {
            v["error"]["type"]
                .as_str()
                .map(|s| s.chars().take(64).collect())
        })
        .map(|t: String| format!(" ({t})"))
        .unwrap_or_default()
}

impl LlmCaller {
    pub fn new(
        cfg: &LlmConfig,
        api_key: &str,
        ledger: SharedLedger,
        clock: Clock,
    ) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            // The key header must never follow a redirect to another host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        Ok(Self {
            http,
            endpoint: format!("{}/v1/messages", cfg.api_base_url.trim_end_matches('/')),
            api_key: api_key.to_string(),
            pricing: cfg.pricing.clone(),
            limits: CallLimits::from_config(cfg),
            ledger,
            clock,
        })
    }

    pub fn ledger(&self) -> &SharedLedger {
        &self.ledger
    }

    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    pub fn limits(&self) -> &CallLimits {
        &self.limits
    }

    /// One logical call, including bounded retries. Always returns a report.
    pub async fn call(&self, req: CallRequest<'_>) -> (LlmCallReport, Result<Value, DeciderError>) {
        let started = Instant::now();
        let mut report = LlmCallReport::new(req.role, req.model);
        let result = self.call_inner(&req, &mut report).await;
        report.latency_ms = started.elapsed().as_millis() as u64;
        report.outcome = match &result {
            Ok(_) => "ok".into(),
            Err(e) => format!("error:{}", e.kind()),
        };
        (report, result)
    }

    async fn call_inner(
        &self,
        req: &CallRequest<'_>,
        report: &mut LlmCallReport,
    ) -> Result<Value, DeciderError> {
        let body = serde_json::to_vec(&req.body).map_err(|e| DeciderError::Other(e.to_string()))?;
        let input_upper = body.len() as u64 + REQUEST_OVERHEAD_TOKENS;
        if input_upper > self.limits.max_input_tokens {
            return Err(DeciderError::PromptTooLarge {
                estimated_tokens: input_upper,
                limit: self.limits.max_input_tokens,
            });
        }
        let Some(price) = self.pricing.get(req.model).copied() else {
            self.ledger.halt("missing_pricing");
            return Err(DeciderError::Pricing(format!("no price for {}", req.model)));
        };
        let reserve_usd = price.max_charge_usd(input_upper, req.max_output_tokens as u64);
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            report.attempts = attempt;
            let remaining = req.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(DeciderError::Stale {
                    detail: "deadline passed before dispatch".into(),
                });
            }
            let timeout = remaining.min(self.limits.request_timeout);
            let now = (self.clock)();
            let call_end =
                now + chrono::Duration::from_std(timeout).unwrap_or(chrono::Duration::MAX);
            let id = self
                .ledger
                .reserve(req.role, req.model, reserve_usd, now, call_end)
                .map_err(budget)?;
            let retryable = match self.send(&body, timeout).await {
                Attempt::Status(200, bytes) => {
                    return self.settle_success(id, &price, req.model, &bytes, report)
                }
                Attempt::Status(status, bytes) => {
                    // An error response from the API is not billed.
                    self.ledger.release(id).map_err(budget)?;
                    let err =
                        DeciderError::Http(format!("status {status}{}", api_error_type(&bytes)));
                    if status == 429 || status >= 500 {
                        err
                    } else {
                        return Err(err);
                    }
                }
                Attempt::NotSent(m) => {
                    self.ledger.release(id).map_err(budget)?;
                    DeciderError::Http(m)
                }
                Attempt::Timeout => {
                    self.ledger
                        .mark_ambiguous(id, "timeout", None)
                        .map_err(budget)?;
                    return Err(DeciderError::Timeout {
                        after_ms: timeout.as_millis() as u64,
                    });
                }
                Attempt::Failed(m) => {
                    self.ledger.mark_ambiguous(id, &m, None).map_err(budget)?;
                    return Err(DeciderError::Http(m));
                }
            };
            if attempt > self.limits.max_retries {
                return Err(retry_exhausted(retryable, attempt));
            }
            let backoff = self
                .limits
                .retry_backoff
                .saturating_mul(1u32 << (attempt - 1).min(16));
            if Instant::now() + backoff >= req.deadline {
                return Err(retry_exhausted(retryable, attempt));
            }
            tokio::time::sleep(backoff).await;
        }
    }

    fn settle_success(
        &self,
        id: u64,
        price: &ModelPrice,
        model: &str,
        bytes: &[u8],
        report: &mut LlmCallReport,
    ) -> Result<Value, DeciderError> {
        let ambiguous = |note: &str, halt: &str| {
            self.ledger
                .mark_ambiguous(id, note, Some(halt))
                .map_err(budget)
        };
        let Ok(v) = serde_json::from_slice::<Value>(bytes) else {
            ambiguous("unparseable 200 response", "missing_usage")?;
            return Err(DeciderError::MissingUsage(
                "response body is not JSON".into(),
            ));
        };
        let Some(usage) = Usage::from_response(&v) else {
            ambiguous("200 without usage", "missing_usage")?;
            return Err(DeciderError::MissingUsage("response has no usage".into()));
        };
        report.usage.add(&usage);
        let served = v.get("model").and_then(Value::as_str).unwrap_or("");
        if !(served == model || served.starts_with(&format!("{model}-"))) {
            ambiguous("response model has no price", "missing_pricing")?;
            return Err(DeciderError::Pricing(format!(
                "served model {served:?} does not match {model}"
            )));
        }
        if usage.prompt_tokens() >= MAX_PROMPT_TOKENS {
            ambiguous("prompt in long-context tier", "missing_pricing")?;
            return Err(DeciderError::Pricing(format!(
                "prompt used {} tokens; long-context pricing is not configured",
                usage.prompt_tokens()
            )));
        }
        let cost = usage.cost_usd(price);
        report.cost_usd += cost;
        self.ledger.settle(id, cost).map_err(budget)?;
        Ok(v)
    }

    async fn send(&self, body: &[u8], timeout: Duration) -> Attempt {
        let max = self.limits.max_response_bytes;
        let fut = async {
            let sent = self
                .http
                .post(&self.endpoint)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .body(body.to_vec())
                .send()
                .await;
            let mut resp = match sent {
                Ok(r) => r,
                Err(e) if e.is_connect() => return Attempt::NotSent("connect failed".into()),
                Err(e) if e.is_timeout() => return Attempt::Timeout,
                Err(_) => return Attempt::Failed("transport error".into()),
            };
            let status = resp.status().as_u16();
            if resp.content_length().is_some_and(|n| n > max as u64) {
                return Attempt::Failed(format!("response larger than {max} bytes"));
            }
            let mut buf = Vec::new();
            loop {
                match resp.chunk().await {
                    Ok(Some(chunk)) => {
                        if buf.len() + chunk.len() > max {
                            return Attempt::Failed(format!("response larger than {max} bytes"));
                        }
                        buf.extend_from_slice(&chunk);
                    }
                    Ok(None) => break,
                    Err(e) if e.is_timeout() => return Attempt::Timeout,
                    Err(_) => return Attempt::Failed("response stream error".into()),
                }
            }
            Attempt::Status(status, buf)
        };
        tokio::time::timeout(timeout, fut)
            .await
            .unwrap_or(Attempt::Timeout)
    }
}

fn retry_exhausted(last: DeciderError, attempts: u32) -> DeciderError {
    match last {
        DeciderError::Http(m) => DeciderError::Http(format!("{m} after {attempts} attempt(s)")),
        other => other,
    }
}

/// Replace control characters, collapse whitespace, and cap length (chars).
pub fn sanitize_text(s: &str, max_chars: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut out: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
    }
    out
}
