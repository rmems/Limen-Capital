//! Ed25519 request signing for the Robinhood Crypto Trading API.
//!
//! Every authenticated request carries three headers:
//!
//! | Header        | Value                                                        |
//! |---------------|--------------------------------------------------------------|
//! | `x-api-key`   | the `rh-api-…` key issued in crypto account settings         |
//! | `x-timestamp` | Unix seconds; the venue accepts it for 30 s                  |
//! | `x-signature` | base64(Ed25519-sign(`api_key + timestamp + path + method + body`)) |
//!
//! `path` includes the query string (e.g. `/api/v2/crypto/marketdata/best_bid_ask/?symbol=BTC-USD`),
//! `method` is upper-case (`GET` / `POST`) and `body` is the **exact** string
//! sent on the wire (empty for GET). The private key is the 32-byte Ed25519
//! seed, base64-encoded, as produced by the key-generation snippet in the docs
//! (`ROBINHOOD_PRIVATE_KEY`).
//!
//! This module is pure: no I/O, no clock. The caller supplies the timestamp.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey, SECRET_KEY_LENGTH};
use std::fmt;

/// Errors from key decoding. Signing itself cannot fail once a key is loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignError {
    /// The base64 private key did not decode.
    InvalidBase64(String),
    /// The decoded seed was not exactly 32 bytes.
    InvalidSeedLength(usize),
}

impl fmt::Display for SignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignError::InvalidBase64(e) => write!(f, "private key is not valid base64: {e}"),
            SignError::InvalidSeedLength(n) => write!(
                f,
                "private key seed must be {SECRET_KEY_LENGTH} bytes, got {n}"
            ),
        }
    }
}

impl std::error::Error for SignError {}

/// The three authentication headers for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthHeaders {
    pub api_key: String,
    pub signature: String,
    pub timestamp: String,
}

impl AuthHeaders {
    /// Header `(name, value)` pairs in the order the docs list them.
    pub fn pairs(&self) -> [(&'static str, &str); 3] {
        [
            ("x-api-key", self.api_key.as_str()),
            ("x-signature", self.signature.as_str()),
            ("x-timestamp", self.timestamp.as_str()),
        ]
    }
}

/// Build the exact UTF-8 message the venue expects to be signed.
///
/// `message = api_key + timestamp + path + method + body`
pub fn signing_message(
    api_key: &str,
    timestamp: u64,
    path: &str,
    method: &str,
    body: &str,
) -> String {
    let mut m = String::with_capacity(api_key.len() + 12 + path.len() + method.len() + body.len());
    m.push_str(api_key);
    m.push_str(&timestamp.to_string());
    m.push_str(path);
    m.push_str(method);
    m.push_str(body);
    m
}

/// Decode a base64 Ed25519 seed (`ROBINHOOD_PRIVATE_KEY`) into a signing key.
pub fn signing_key_from_b64(seed_b64: &str) -> Result<SigningKey, SignError> {
    let bytes = B64
        .decode(seed_b64.trim())
        .map_err(|e| SignError::InvalidBase64(e.to_string()))?;
    let seed: [u8; SECRET_KEY_LENGTH] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| SignError::InvalidSeedLength(bytes.len()))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Decode a base64 Ed25519 public key (the value registered with Robinhood).
pub fn verifying_key_from_b64(pub_b64: &str) -> Result<VerifyingKey, SignError> {
    let bytes = B64
        .decode(pub_b64.trim())
        .map_err(|e| SignError::InvalidBase64(e.to_string()))?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| SignError::InvalidSeedLength(bytes.len()))?;
    VerifyingKey::from_bytes(&arr).map_err(|e| SignError::InvalidBase64(e.to_string()))
}

/// Sign one request and return the base64 `x-signature` value.
///
/// Pure function: same inputs → same output. See [`signing_message`] for the
/// message layout. `body` must be byte-for-byte what is sent on the wire.
pub fn sign(
    key: &SigningKey,
    api_key: &str,
    timestamp: u64,
    path: &str,
    method: &str,
    body: &str,
) -> String {
    let message = signing_message(api_key, timestamp, path, method, body);
    let sig = key.sign(message.as_bytes());
    B64.encode(sig.to_bytes())
}

/// Convenience: decode the seed and sign in one call.
pub fn sign_with_b64_seed(
    seed_b64: &str,
    api_key: &str,
    timestamp: u64,
    path: &str,
    method: &str,
    body: &str,
) -> Result<String, SignError> {
    let key = signing_key_from_b64(seed_b64)?;
    Ok(sign(&key, api_key, timestamp, path, method, body))
}

/// Holds the API key and signing key; produces [`AuthHeaders`] per request.
///
/// `Debug` deliberately omits the key material.
#[derive(Clone)]
pub struct RequestSigner {
    api_key: String,
    key: SigningKey,
}

impl fmt::Debug for RequestSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestSigner")
            .field("api_key", &redact(&self.api_key))
            .field("key", &"<redacted>")
            .finish()
    }
}

impl RequestSigner {
    pub fn new(api_key: impl Into<String>, seed_b64: &str) -> Result<Self, SignError> {
        Ok(Self {
            api_key: api_key.into(),
            key: signing_key_from_b64(seed_b64)?,
        })
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Base64 public key, for comparing against what is registered at the venue.
    pub fn public_key_b64(&self) -> String {
        B64.encode(self.key.verifying_key().to_bytes())
    }

    /// Headers for a request at `timestamp` (Unix seconds).
    pub fn headers(&self, timestamp: u64, path: &str, method: &str, body: &str) -> AuthHeaders {
        AuthHeaders {
            api_key: self.api_key.clone(),
            signature: sign(&self.key, &self.api_key, timestamp, path, method, body),
            timestamp: timestamp.to_string(),
        }
    }
}

/// Show only a short prefix of a secret-ish identifier.
pub fn redact(s: &str) -> String {
    const KEEP: usize = 10;
    if s.chars().count() <= KEEP {
        return "…".to_string();
    }
    let kept: String = s.chars().take(KEEP).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier};

    // Worked example from docs.robinhood.com/crypto/trading → Authentication →
    // "Example Signature" (verified 2026-11-08). The docs' Python snippet
    // builds the message with f"{...}{body}" where `body` is a dict, so the
    // signed string is Python's dict repr, and that is what the published
    // signature header corresponds to. The venue itself verifies against the
    // exact body bytes you send; the client always signs the serialized JSON
    // it transmits (see the Python reference client's `make_api_request`).
    const DOC_PRIVATE_KEY: &str = "xQnTJVeQLmw1/Mg2YimEViSpw/SdJcgNXZ5kQkAXNPU=";
    const DOC_PUBLIC_KEY: &str = "jPItx4TLjcnSUnmnXQQyAKL4eJj3+oWNNMmmm2vATqk=";
    const DOC_API_KEY: &str = "rh-api-6148effc-c0b1-486c-8940-a1d099456be6";
    const DOC_TIMESTAMP: u64 = 1698708981;
    const DOC_PATH: &str = "/api/v1/crypto/trading/orders/";
    const DOC_METHOD: &str = "POST";
    const DOC_BODY_PY_REPR: &str = "{'client_order_id': '131de903-5a9c-4260-abc1-28d562a5dcf0', 'side': 'buy', 'symbol': 'BTC-USD', 'type': 'market', 'market_order_config': {'asset_quantity': '0.1'}}";
    const DOC_SIGNATURE: &str =
        "q/nEtxp/P2Or3hph3KejBqnw5o9qeuQ+hYRnB56FaHbjDsNUY9KhB1asMxohDnzdVFSD7StaTqjSd9U9HvaRAw==";

    #[test]
    fn docs_worked_example_reproduces_published_signature() {
        let sig = sign_with_b64_seed(
            DOC_PRIVATE_KEY,
            DOC_API_KEY,
            DOC_TIMESTAMP,
            DOC_PATH,
            DOC_METHOD,
            DOC_BODY_PY_REPR,
        )
        .unwrap();
        assert_eq!(sig, DOC_SIGNATURE);
    }

    #[test]
    fn docs_private_key_derives_docs_public_key() {
        let signer = RequestSigner::new(DOC_API_KEY, DOC_PRIVATE_KEY).unwrap();
        assert_eq!(signer.public_key_b64(), DOC_PUBLIC_KEY);
    }

    #[test]
    fn signing_message_layout_is_key_ts_path_method_body() {
        let m = signing_message("K", 42, "/p/", "GET", "");
        assert_eq!(m, "K42/p/GET");
        let m = signing_message("K", 42, "/p/?symbol=BTC-USD", "POST", "{\"a\":1}");
        assert_eq!(m, "K42/p/?symbol=BTC-USDPOST{\"a\":1}");
    }

    #[test]
    fn headers_carry_all_three_fields_and_verify() {
        let signer = RequestSigner::new(DOC_API_KEY, DOC_PRIVATE_KEY).unwrap();
        let path = "/api/v2/crypto/marketdata/best_bid_ask/?symbol=BTC-USD";
        let h = signer.headers(1_700_000_000, path, "GET", "");
        assert_eq!(h.api_key, DOC_API_KEY);
        assert_eq!(h.timestamp, "1700000000");
        let names: Vec<&str> = h.pairs().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["x-api-key", "x-signature", "x-timestamp"]);

        // Independent verification with the registered public key.
        let vk = verifying_key_from_b64(DOC_PUBLIC_KEY).unwrap();
        let sig_bytes = B64.decode(&h.signature).unwrap();
        let sig = Signature::from_slice(&sig_bytes).unwrap();
        let msg = signing_message(DOC_API_KEY, 1_700_000_000, path, "GET", "");
        vk.verify(msg.as_bytes(), &sig).unwrap();
    }

    #[test]
    fn signature_changes_with_every_input() {
        let key = signing_key_from_b64(DOC_PRIVATE_KEY).unwrap();
        let base = sign(&key, "k", 1, "/p/", "GET", "");
        assert_ne!(base, sign(&key, "k2", 1, "/p/", "GET", ""));
        assert_ne!(base, sign(&key, "k", 2, "/p/", "GET", ""));
        assert_ne!(base, sign(&key, "k", 1, "/q/", "GET", ""));
        assert_ne!(base, sign(&key, "k", 1, "/p/", "POST", ""));
        assert_ne!(base, sign(&key, "k", 1, "/p/", "GET", "{}"));
        // Deterministic (Ed25519 is), so retries at the same timestamp re-sign identically.
        assert_eq!(base, sign(&key, "k", 1, "/p/", "GET", ""));
    }

    #[test]
    fn rejects_bad_base64() {
        let err = signing_key_from_b64("not*base64!").unwrap_err();
        assert!(matches!(err, SignError::InvalidBase64(_)));
        assert!(RequestSigner::new("k", "not*base64!").is_err());
    }

    #[test]
    fn rejects_wrong_seed_length() {
        let short = B64.encode([1u8; 16]);
        assert_eq!(
            signing_key_from_b64(&short).unwrap_err(),
            SignError::InvalidSeedLength(16)
        );
        // A 64-byte "expanded" key (pynacl SigningKey + verify key) is also rejected:
        // only the 32-byte seed is accepted, matching the docs' key format.
        let long = B64.encode([1u8; 64]);
        assert_eq!(
            signing_key_from_b64(&long).unwrap_err(),
            SignError::InvalidSeedLength(64)
        );
    }

    #[test]
    fn trims_whitespace_around_env_value() {
        let signer = RequestSigner::new(DOC_API_KEY, &format!("  {DOC_PRIVATE_KEY}\n")).unwrap();
        assert_eq!(signer.public_key_b64(), DOC_PUBLIC_KEY);
    }

    #[test]
    fn redact_is_char_boundary_safe() {
        assert_eq!(redact("short"), "…");
        assert_eq!(redact("rh-api-6148effc-c0b1"), "rh-api-614…");
        // Multi-byte characters around the cut must not panic.
        assert_eq!(redact("ééééééééééééé"), "éééééééééé…");
    }

    #[test]
    fn debug_does_not_leak_key_material() {
        let signer = RequestSigner::new(DOC_API_KEY, DOC_PRIVATE_KEY).unwrap();
        let dbg = format!("{signer:?}");
        assert!(!dbg.contains(DOC_PRIVATE_KEY));
        assert!(!dbg.contains(DOC_API_KEY));
        assert!(dbg.contains("rh-api-614…"));
    }
}
