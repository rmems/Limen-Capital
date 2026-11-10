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

fn json_u64(v: &Value, key: &str) -> Option<u64> {
    v.get(key)?.as_u64()
}

/// Missing or explicit-null cache counts mean zero.
fn json_u64_or_zero(v: &Value, key: &str) -> Option<u64> {
    match v.get(key) {
        None | Some(Value::Null) => Some(0),
        Some(x) => x.as_u64(),
    }
}

impl Usage {
    /// `None` when `input_tokens` / `output_tokens` are missing or malformed.
    pub fn from_response(v: &Value) -> Option<Self> {
        let u = v.get("usage")?;
        Some(Self {
            input_tokens: json_u64(u, "input_tokens")?,
            output_tokens: json_u64(u, "output_tokens")?,
            cache_read_input_tokens: json_u64_or_zero(u, "cache_read_input_tokens")?,
            cache_creation_input_tokens: json_u64_or_zero(u, "cache_creation_input_tokens")?,
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

/// Transport outcome of a single attempt.
enum Attempt {
    Status(u16, Vec<u8>),
    /// Failed before the request reached the API: definitely not charged.
    NotSent(String),
    Timeout,
    /// Failed after sending: outcome unknown.
    Failed(String),
}

/// What one attempt means for the retry loop.
enum Outcome {
    Done(Result<Value, DeciderError>),
    Retry(DeciderError),
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
        let reserve_usd = price.max_charge_usd(input_upper, u64::from(req.max_output_tokens));
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            report.attempts = attempt;
            let retryable = match self.attempt(req, &body, &price, reserve_usd, report).await {
                Outcome::Done(result) => return result,
                Outcome::Retry(e) => e,
            };
            let backoff = self.backoff_or_exhausted(retryable, attempt, req.deadline)?;
            tokio::time::sleep(backoff).await;
        }
    }

    /// Backoff before the next attempt, or the final error when retries are
    /// exhausted or the deadline cannot fit another attempt.
    fn backoff_or_exhausted(
        &self,
        err: DeciderError,
        attempt: u32,
        deadline: Instant,
    ) -> Result<Duration, DeciderError> {
        if attempt > self.limits.max_retries {
            return Err(retry_exhausted(err, attempt));
        }
        let backoff = self
            .limits
            .retry_backoff
            .saturating_mul(1u32 << (attempt - 1).min(16));
        if Instant::now() + backoff >= deadline {
            return Err(retry_exhausted(err, attempt));
        }
        Ok(backoff)
    }

    /// Reserve budget, dispatch once, and classify the result.
    async fn attempt(
        &self,
        req: &CallRequest<'_>,
        body: &[u8],
        price: &ModelPrice,
        reserve_usd: f64,
        report: &mut LlmCallReport,
    ) -> Outcome {
        let remaining = req.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Outcome::Done(Err(DeciderError::Stale {
                detail: "deadline passed before dispatch".into(),
            }));
        }
        let timeout = remaining.min(self.limits.request_timeout);
        let id = match self.reserve(req, reserve_usd, timeout) {
            Ok(id) => id,
            Err(e) => return Outcome::Done(Err(e)),
        };
        match self.send(body, timeout).await {
            Attempt::Status(200, bytes) => {
                Outcome::Done(self.settle_success(id, price, req.model, &bytes, report))
            }
            other => self.unsuccessful(id, other, timeout),
        }
    }

    /// Reserve the worst-case charge; the call must end before credit expiry.
    fn reserve(
        &self,
        req: &CallRequest<'_>,
        reserve_usd: f64,
        timeout: Duration,
    ) -> Result<u64, DeciderError> {
        let now = (self.clock)();
        let call_end = now + chrono::Duration::from_std(timeout).unwrap_or(chrono::Duration::MAX);
        self.ledger
            .reserve(req.role, req.model, reserve_usd, now, call_end)
            .map_err(budget)
    }

    fn unsuccessful(&self, id: u64, attempt: Attempt, timeout: Duration) -> Outcome {
        match attempt {
            Attempt::Status(status, bytes) => self.error_status(id, status, &bytes),
            Attempt::NotSent(m) => self.release_outcome(id, DeciderError::Http(m)),
            Attempt::Timeout => self.ambiguous_outcome(
                id,
                "timeout",
                DeciderError::Timeout {
                    after_ms: timeout.as_millis() as u64,
                },
            ),
            Attempt::Failed(m) => {
                let note = m.clone();
                self.ambiguous_outcome(id, &note, DeciderError::Http(m))
            }
        }
    }

    /// An API error response is not billed; only 429/5xx may be retried.
    fn error_status(&self, id: u64, status: u16, bytes: &[u8]) -> Outcome {
        if let Err(e) = self.ledger.release(id).map_err(budget) {
            return Outcome::Done(Err(e));
        }
        let err = DeciderError::Http(format!("status {status}{}", api_error_type(bytes)));
        if status == 429 || status >= 500 {
            Outcome::Retry(err)
        } else {
            Outcome::Done(Err(err))
        }
    }

    /// The request never reached the API: release the reservation and retry.
    fn release_outcome(&self, id: u64, err: DeciderError) -> Outcome {
        match self.ledger.release(id).map_err(budget) {
            Ok(()) => Outcome::Retry(err),
            Err(e) => Outcome::Done(Err(e)),
        }
    }

    /// The request may have been charged: keep the reservation counted.
    fn ambiguous_outcome(&self, id: u64, note: &str, err: DeciderError) -> Outcome {
        match self.ledger.mark_ambiguous(id, note, None).map_err(budget) {
            Ok(()) => Outcome::Done(Err(err)),
            Err(e) => Outcome::Done(Err(e)),
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
        let v = self.ok_json(id, bytes)?;
        let usage = self.required_usage(id, &v)?;
        report.usage.add(&usage);
        self.verify_priced(id, &v, model, &usage)?;
        let cost = usage.cost_usd(price);
        report.cost_usd += cost;
        self.ledger.settle(id, cost).map_err(budget)?;
        Ok(v)
    }

    /// Keep the reservation and halt paid calls: a 200 we cannot bill
    /// correctly must never silently reset the meter.
    fn keep_and_halt(&self, id: u64, note: &str, halt: &str) -> Result<(), DeciderError> {
        self.ledger
            .mark_ambiguous(id, note, Some(halt))
            .map_err(budget)
    }

    fn ok_json(&self, id: u64, bytes: &[u8]) -> Result<Value, DeciderError> {
        let Ok(v) = serde_json::from_slice::<Value>(bytes) else {
            self.keep_and_halt(id, "unparseable 200 response", "missing_usage")?;
            return Err(DeciderError::MissingUsage(
                "response body is not JSON".into(),
            ));
        };
        Ok(v)
    }

    fn required_usage(&self, id: u64, v: &Value) -> Result<Usage, DeciderError> {
        let Some(usage) = Usage::from_response(v) else {
            self.keep_and_halt(id, "200 without usage", "missing_usage")?;
            return Err(DeciderError::MissingUsage("response has no usage".into()));
        };
        Ok(usage)
    }

    /// The served model must be the priced one, and the prompt must be inside
    /// the priced under-100k tier.
    fn verify_priced(
        &self,
        id: u64,
        v: &Value,
        model: &str,
        usage: &Usage,
    ) -> Result<(), DeciderError> {
        let served = v.get("model").and_then(Value::as_str).unwrap_or("");
        if !(served == model || served.starts_with(&format!("{model}-"))) {
            self.keep_and_halt(id, "response model has no price", "missing_pricing")?;
            return Err(DeciderError::Pricing(format!(
                "served model {served:?} does not match {model}"
            )));
        }
        if usage.prompt_tokens() >= MAX_PROMPT_TOKENS {
            self.keep_and_halt(id, "prompt in long-context tier", "missing_pricing")?;
            return Err(DeciderError::Pricing(format!(
                "prompt used {} tokens; long-context pricing is not configured",
                usage.prompt_tokens()
            )));
        }
        Ok(())
    }

    async fn send(&self, body: &[u8], timeout: Duration) -> Attempt {
        tokio::time::timeout(timeout, self.exchange(body))
            .await
            .unwrap_or(Attempt::Timeout)
    }

    async fn exchange(&self, body: &[u8]) -> Attempt {
        let resp = match self.post(body).await {
            Ok(resp) => resp,
            Err(attempt) => return attempt,
        };
        let status = resp.status().as_u16();
        match self.read_body(resp).await {
            Ok(bytes) => Attempt::Status(status, bytes),
            Err(attempt) => attempt,
        }
    }

    async fn post(&self, body: &[u8]) -> Result<reqwest::Response, Attempt> {
        self.http
            .post(&self.endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    Attempt::NotSent("connect failed".into())
                } else if e.is_timeout() {
                    Attempt::Timeout
                } else {
                    Attempt::Failed("transport error".into())
                }
            })
    }

    /// Read at most `max_response_bytes`; an oversize or broken stream is an
    /// ambiguous failure (the request already reached the API).
    async fn read_body(&self, mut resp: reqwest::Response) -> Result<Vec<u8>, Attempt> {
        let max = self.limits.max_response_bytes;
        if resp.content_length().is_some_and(|n| n > max as u64) {
            return Err(self.too_large());
        }
        let mut buf = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if buf.len() + chunk.len() > max {
                        return Err(self.too_large());
                    }
                    buf.extend_from_slice(&chunk);
                }
                Ok(None) => return Ok(buf),
                Err(e) if e.is_timeout() => return Err(Attempt::Timeout),
                Err(_) => return Err(Attempt::Failed("response stream error".into())),
            }
        }
    }

    fn too_large(&self) -> Attempt {
        Attempt::Failed(format!(
            "response larger than {} bytes",
            self.limits.max_response_bytes
        ))
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
