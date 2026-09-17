//! Baseline snapshot source: a user's account state at block H as borsh bytes
//! of an `AccountVersioned`, for the engine's `Baseline.raw_account`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use sweat_jar_model::data::account::{versioned::AccountVersioned, Account};

/// Public FastNEAR archival JSON-RPC endpoint.
pub const FASTNEAR_ARCHIVAL_RPC: &str = "https://archival-rpc.mainnet.fastnear.com";

/// Env var holding the archival-RPC API key, sent as `Authorization: Bearer …`.
/// Unset = unauthenticated (the free tier, which throttles hard above ~4
/// concurrent requests).
pub const API_KEY_ENV: &str = "FASTNEAR_API_KEY";

/// Reads [`API_KEY_ENV`], treating blank/whitespace as unset.
pub fn api_key_from_env() -> Option<String> {
    std::env::var(API_KEY_ENV)
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// Source of a user's account state at block H (borsh bytes of an `AccountVersioned`), or `None`.
pub trait SnapshotSource: Send + Sync {
    /// `account_id` is the integer replay id; `near_account_id` is the on-chain
    /// account (64-hex or named). Returns the raw borsh account bytes for the
    /// engine's `Baseline.raw_account`, or `Ok(None)` if the account had no
    /// state at block H.
    fn raw_account(&self, account_id: i64, near_account_id: &str) -> Result<Option<Vec<u8>>>;

    /// Whether `Ok(None)` from this source means "confirmed no state at H" (a
    /// complete answer — the account is genuinely fresh, its first `Deposit`
    /// creates it, exactly as the real contract's `get_or_create_account_mut`
    /// does) rather than "this source doesn't know" (a gap — `no_baseline`).
    fn is_authoritative(&self) -> bool;

    /// Re-fetch state at an arbitrary historical block height — the fallback
    /// for an account whose block-H baseline is confirmed empty but whose
    /// first action isn't a `Deposit` (the only action that auto-creates an
    /// account; anything else as the first action means something invisible
    /// to the tracked event types created it — e.g. an FT-transfer migration
    /// from a previous contract version, which writes storage directly and
    /// emits no event at all). Querying at that first event's own block
    /// height returns state as of right after it executed, so the caller
    /// should drop that leading event before replaying against the result.
    ///
    /// Default: unsupported (`Ok(None)`) — only a live archival RPC can query
    /// an arbitrary height; a `None` here is never treated as "confirmed
    /// empty", just "didn't try".
    fn raw_account_at(&self, _near_account_id: &str, _block_height: u64) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

/// Reads the `snapshots` table of a replay DB.
pub struct DbSnapshotSource {
    db_path: PathBuf,
}

impl DbSnapshotSource {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self { db_path: db_path.into() }
    }
}

impl SnapshotSource for DbSnapshotSource {
    fn raw_account(&self, account_id: i64, _near_account_id: &str) -> Result<Option<Vec<u8>>> {
        let conn = crate::db::open_read(&self.db_path)?;
        let mut stmt = conn
            .prepare("SELECT state_json FROM snapshots WHERE backend_account_id = ?")
            .context("prepare snapshots query")?;
        let mut rows = stmt
            .query_map(duckdb::params![account_id], |r| r.get::<_, String>(0))
            .context("query snapshots table")?;
        match rows.next() {
            Some(row) => {
                let state_json = row.context("read snapshots row")?;
                account_state_json_to_raw(&state_json).map(Some)
            }
            None => Ok(None),
        }
    }

    /// The `snapshots` table is a local cache, typically unpopulated — a miss
    /// only means "we didn't fetch one", not "confirmed absent".
    fn is_authoritative(&self) -> bool {
        false
    }
}

/// Fetches account state at block H from a NEAR archival node by calling the jar
/// contract's `get_account` view against historical state.
///
/// (`view_state` over the whole contract is refused by public archival nodes —
/// "state too large" — so a per-account view call is the only route.)
pub struct ArchivalRpcSnapshotSource {
    pub rpc_url: String,
    pub jar_contract: String,
    pub block_height: u64,
    /// Sent as `Authorization: Bearer <key>`. `None` = unauthenticated.
    /// Never logged — error messages carry the URL only.
    pub api_key: Option<String>,
}

impl ArchivalRpcSnapshotSource {
    /// Point at FastNEAR's public archival RPC, keyed from [`API_KEY_ENV`] if set.
    pub fn fastnear(jar_contract: impl Into<String>, block_height: u64) -> Self {
        Self {
            rpc_url: FASTNEAR_ARCHIVAL_RPC.to_string(),
            jar_contract: jar_contract.into(),
            block_height,
            api_key: api_key_from_env(),
        }
    }

    /// Build the `query`/`call_function` request body for `get_account(near_id)`
    /// at an arbitrary block height.
    fn request_body(&self, near_account_id: &str, block_height: u64) -> near_sdk::serde_json::Value {
        use base64::prelude::{Engine, BASE64_STANDARD};
        let args = near_sdk::serde_json::json!({ "account_id": near_account_id });
        let args_base64 = BASE64_STANDARD.encode(near_sdk::serde_json::to_vec(&args).unwrap());
        near_sdk::serde_json::json!({
            "jsonrpc": "2.0", "id": "1", "method": "query",
            "params": {
                "request_type": "call_function",
                "block_id": block_height,
                "account_id": self.jar_contract,
                "method_name": "get_account",
                "args_base64": args_base64,
            }
        })
    }

    /// Shared fetch path for both the fixed block-H query and the
    /// arbitrary-height fallback.
    fn fetch_at(&self, near_account_id: &str, block_height: u64) -> Result<Option<Vec<u8>>> {
        let resp = http_post_json(&self.rpc_url, &self.request_body(near_account_id, block_height), self.api_key.as_deref())
            .with_context(|| format!("get_account({near_account_id}) @ block {block_height}"))?;
        parse_get_account_response(&resp)
            .with_context(|| format!("parsing get_account({near_account_id}) response"))
    }
}

/// Parse a `query`/`call_function` RPC response for `get_account` into borsh
/// account bytes. `Ok(None)` when the method returned `null` (no account at H).
fn parse_get_account_response(resp: &near_sdk::serde_json::Value) -> Result<Option<Vec<u8>>> {
    if let Some(err) = resp.get("error") {
        anyhow::bail!("archival RPC error: {err}");
    }
    let result = resp.get("result").context("RPC response has no `result`")?;
    if let Some(err) = result.get("error").and_then(|e| e.as_str()) {
        anyhow::bail!("get_account failed: {err}");
    }
    let bytes: Vec<u8> = near_sdk::serde_json::from_value(
        result.get("result").context("`result.result` missing")?.clone(),
    )
    .context("`result.result` is not a byte array")?;
    let json = String::from_utf8(bytes).context("get_account result is not UTF-8")?;
    let state: near_sdk::serde_json::Value =
        near_sdk::serde_json::from_str(&json).context("get_account result is not JSON")?;
    if state.is_null() {
        return Ok(None);
    }
    Ok(Some(account_state_value_to_raw(&state)?))
}

fn http_post_json(
    url: &str,
    body: &near_sdk::serde_json::Value,
    api_key: Option<&str>,
) -> Result<near_sdk::serde_json::Value> {
    let attempts = 3u64;
    let mut last: Option<String> = None;
    for attempt in 0..attempts {
        let mut req = ureq::post(url).timeout(std::time::Duration::from_secs(30));
        if let Some(key) = api_key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        match req.send_json(body) {
            Ok(r) => return r.into_json().context("decode RPC response body"),
            // 429 is a rate limit, not a malformed request — retry it like any
            // transient error instead of bailing immediately (a bad key/expired
            // key otherwise floods the worklist with instant `failure:` rows
            // and, worse, floods the writer's channel just as fast — see
            // run.rs's bounded channel comment).
            Err(ureq::Error::Status(429, _)) => {
                last = Some("HTTP 429".to_string());
                if attempt + 1 < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(1000 * (attempt + 1)));
                }
            }
            // Any other 4xx is a bug on our side (bad request shape) — don't retry it.
            Err(ureq::Error::Status(code, _)) if (400..500).contains(&code) => {
                anyhow::bail!("RPC POST to {url} rejected with HTTP {code}");
            }
            Err(e) => {
                last = Some(e.to_string());
                if attempt + 1 < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(250 * (attempt + 1)));
                }
            }
        }
    }
    anyhow::bail!("RPC POST to {url} failed after {attempts} attempts: {}", last.unwrap_or_default())
}

impl SnapshotSource for ArchivalRpcSnapshotSource {
    fn raw_account(&self, _account_id: i64, near_account_id: &str) -> Result<Option<Vec<u8>>> {
        self.fetch_at(near_account_id, self.block_height)
    }

    /// A live `get_account` call is a definitive on-chain answer: `Ok(None)`
    /// means the account genuinely had no state at block H.
    fn is_authoritative(&self) -> bool {
        true
    }

    /// A real archival query, parametrized to an arbitrary height.
    fn raw_account_at(&self, near_account_id: &str, block_height: u64) -> Result<Option<Vec<u8>>> {
        self.fetch_at(near_account_id, block_height)
    }
}

/// Whether any jar in these raw account bytes is mid-lock (`is_locked`).
/// `Restake`/`Claim`/`Withdraw`/`WithdrawAll` all lock the jars they touch
/// before sending a transfer promise, and only unlock (and emit their event)
/// from the callback — so state fetched at a block during that window is a
/// transient snapshot of whichever one of those operations is still in
/// flight, not a genuinely resolved prior state. Every raw-account encoder in
/// this module always produces `AccountVersioned::V2` (`account_state_value_to_raw`),
/// so the match is exhaustive in practice; any other shape (a decode failure)
/// conservatively reports "not locked" — the caller treats that candidate as
/// usable rather than looping forever on a source that can't decode.
pub fn any_jar_locked(raw: &[u8]) -> bool {
    match near_sdk::borsh::from_slice::<AccountVersioned>(raw) {
        Ok(AccountVersioned::V2(account)) => account.jars.values().any(|j| j.is_locked),
        _ => false,
    }
}

/// `account_state` JSON value -> borsh(`AccountVersioned`) bytes.
/// `crate::parse::H_MS` is the `score.updated_at` fallback (see `engine::parse_account_state`).
fn account_state_value_to_raw(state: &near_sdk::serde_json::Value) -> Result<Vec<u8>> {
    let account: Account = sweat_jar::replay::engine::parse_account_state(state, crate::parse::H_MS);
    near_sdk::borsh::to_vec(&AccountVersioned::new(account)).context("borsh-encode AccountVersioned")
}

/// `account_state` JSON (as a string) -> borsh(`AccountVersioned`) bytes.
pub fn account_state_json_to_raw(state_json: &str) -> Result<Vec<u8>> {
    let v: near_sdk::serde_json::Value =
        near_sdk::serde_json::from_str(state_json).context("snapshot account_state is not valid JSON")?;
    account_state_value_to_raw(&v)
}

#[cfg(test)]
mod tests {
    use near_sdk::borsh::BorshDeserialize;
    use sweat_jar_model::data::account::versioned::AccountVersioned;

    use super::*;

    #[test]
    fn json_snapshot_round_trips_to_borsh() {
        let line = r#"{"near_account_id":"9b6b8403e3ccbd6ba868c58d3e583939f62ca3abc3b2fa22f72b5030cd04efa8","account_state":{"nonce":1,"jars":{},"score":{"updated_at":1774017710156,"history":[{"value":0,"booster":0},{"value":0,"booster":0}]},"features":{"increased_score_cap":false,"increased_apy":true},"timezone":0},"products_referenced":[]}"#;
        let v: near_sdk::serde_json::Value = near_sdk::serde_json::from_str(line).unwrap();
        let raw = account_state_json_to_raw(&v["account_state"].to_string()).unwrap();
        assert!(!raw.is_empty());
        AccountVersioned::try_from_slice(&raw).unwrap();
    }

    fn rpc_reply(inner_json: &str) -> near_sdk::serde_json::Value {
        near_sdk::serde_json::json!({
            "jsonrpc": "2.0", "id": "1",
            "result": {
                "result": inner_json.as_bytes().to_vec(),
                "block_height": 190_375_496,
                "block_hash": "x",
            }
        })
    }

    #[test]
    fn archival_response_parses_account_state_to_borsh() {
        // The exact shape `get_account` returns (see account_full_state_190375496.json).
        let state = r#"{"nonce":152,"jars":{"365d_12apy":{"deposits":[["1761461565082","500000000000000000000000"]],"cache":{"updated_at":"1773990400961","interest":"267832431324931506849"},"is_pending_withdraw":false,"claim_remainder":"25920000000"}},"score":{"updated_at":1773990400961,"history":[{"value":5992,"booster":0},{"value":8124,"booster":0}]},"is_penalty_applied":false,"features":{"increased_score_cap":false,"increased_apy":true},"timezone":0}"#;
        let bytes = parse_get_account_response(&rpc_reply(state))
            .unwrap()
            .expect("account present");
        AccountVersioned::try_from_slice(&bytes).unwrap();
    }

    #[test]
    fn archival_null_account_is_none() {
        assert!(parse_get_account_response(&rpc_reply("null")).unwrap().is_none());
    }

    #[test]
    fn archival_rpc_error_is_err() {
        let resp = near_sdk::serde_json::json!({
            "jsonrpc": "2.0", "id": "1",
            "error": { "name": "HANDLER_ERROR", "cause": { "name": "UNKNOWN_BLOCK" } }
        });
        assert!(parse_get_account_response(&resp).is_err());
    }

    #[test]
    fn archival_request_body_shape() {
        let src = ArchivalRpcSnapshotSource::fastnear("v2.jars.sweat", crate::parse::H_BLOCK);
        let body = src.request_body("abc123", crate::parse::H_BLOCK);
        let p = &body["params"];
        assert_eq!(p["request_type"], "call_function");
        assert_eq!(p["method_name"], "get_account");
        assert_eq!(p["block_id"], crate::parse::H_BLOCK);
        assert_eq!(p["account_id"], "v2.jars.sweat");
        // args_base64 decodes to {"account_id":"abc123"}
        use base64::prelude::{Engine, BASE64_STANDARD};
        let args = BASE64_STANDARD.decode(p["args_base64"].as_str().unwrap()).unwrap();
        assert_eq!(near_sdk::serde_json::from_slice::<near_sdk::serde_json::Value>(&args).unwrap()["account_id"], "abc123");
    }

    #[test]
    fn api_key_env_blank_is_none() {
        // Not asserting on the real env var (tests share a process); this pins
        // the blank/whitespace-is-unset rule the getter applies.
        let normalize = |raw: &str| -> Option<String> {
            Some(raw.trim().to_string()).filter(|k| !k.is_empty())
        };
        assert_eq!(normalize("  "), None);
        assert_eq!(normalize(""), None);
        assert_eq!(normalize(" key123 "), Some("key123".to_string()));
    }

    #[test]
    fn archival_error_context_never_carries_the_api_key() {
        // Unroutable address so the request fails fast; the key must not appear
        // in the error chain (it travels as a header, never in URL or context).
        let src = ArchivalRpcSnapshotSource {
            rpc_url: "http://127.0.0.1:1/".to_string(),
            jar_contract: "v2.jars.sweat".to_string(),
            block_height: crate::parse::H_BLOCK,
            api_key: Some("super-secret-key".to_string()),
        };
        let err = src.raw_account(1, "abc123").unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            !rendered.contains("super-secret-key"),
            "api key leaked into error: {rendered}"
        );
    }

    #[test]
    fn parse_account_state_stamps_zero_updated_at_to_window_start() {
        let v: near_sdk::serde_json::Value = near_sdk::serde_json::from_str(
            r#"{"jars":{},"score":{"updated_at":0,"history":[{"value":10,"booster":0}]},"features":{}}"#,
        )
        .unwrap();
        let account = sweat_jar::replay::engine::parse_account_state(&v, crate::parse::H_MS);
        assert_eq!(account.score.updated_at(), crate::parse::H_MS);
    }

    #[test]
    fn parse_account_state_stamps_missing_updated_at_to_window_start() {
        let v: near_sdk::serde_json::Value =
            near_sdk::serde_json::from_str(r#"{"jars":{},"score":{"history":[]},"features":{}}"#).unwrap();
        let account = sweat_jar::replay::engine::parse_account_state(&v, crate::parse::H_MS);
        assert_eq!(account.score.updated_at(), crate::parse::H_MS);
    }
}
