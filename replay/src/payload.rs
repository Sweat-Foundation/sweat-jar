//! Parse the JSON `payload` string of an on-chain event row into a typed
//! [`ParsedEvent`]. Returns `Ok(None)` for rows that map to no engine action.

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::parse::yocto_str_to_u128;

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedEvent {
    RecordScore(Vec<(u16, u64)>),
    ApplyBooster { score: u16, timestamp_ms: u64 },
    Deposit { product_id: String, amount: u128 },
    WithdrawAll { product_ids: Vec<String> },
    Restake { into: String, from: Vec<String>, restaked: u128 },
    SetIncreasedScoreCap(bool),
    Claim { total: u128 },
}

/// `role` is the event row's `role` column (only meaningful for `apply_booster`).
pub fn parse_event(event: &str, role: Option<&str>, payload: &str) -> Result<Option<ParsedEvent>> {
    let v: Value = serde_json::from_str(payload)
        .with_context(|| format!("payload not JSON for {event}: {payload:?}"))?;
    match event {
        "record_score" => {
            let pairs = v.as_array().context("record_score payload not an array")?;
            let mut out = Vec::with_capacity(pairs.len());
            for p in pairs {
                let a = p.as_array().context("record_score pair not an array")?;
                let score = u16::try_from(a[0].as_u64().context("score not u64")?.min(u16::MAX.into()))
                    .unwrap_or(u16::MAX);
                let ts = a[1].as_u64().context("score ts not u64")?;
                out.push((score, ts));
            }
            Ok((!out.is_empty()).then_some(ParsedEvent::RecordScore(out)))
        }
        "apply_booster" => {
            if role != Some("applied") {
                return Ok(None);
            }
            let score = str_num_u16(&v["score"]).context("booster score")?;
            let timestamp_ms = str_num_u64(&v["timestamp"]).context("booster timestamp")?;
            Ok(Some(ParsedEvent::ApplyBooster { score, timestamp_ms }))
        }
        "deposit" => {
            let inner = &v[1];
            Ok(Some(ParsedEvent::Deposit {
                product_id: inner[0].as_str().context("deposit product_id")?.to_string(),
                amount: yocto_str_to_u128(inner[1].as_str().context("deposit amount")?)?,
            }))
        }
        "withdraw_all" => {
            let rows = v[1].as_array().context("withdraw_all list")?;
            let product_ids = rows
                .iter()
                .map(|r| r[0].as_str().map(str::to_string).context("withdraw_all product_id"))
                .collect::<Result<Vec<_>>>()?;
            Ok(Some(ParsedEvent::WithdrawAll { product_ids }))
        }
        "restake" => {
            let d = &v[1];
            if !d["is_success"].as_bool().unwrap_or(false) {
                return Ok(None);
            }
            let from = d["from"]
                .as_array()
                .context("restake from")?
                .iter()
                .map(|s| s.as_str().map(str::to_string).context("restake from item"))
                .collect::<Result<Vec<_>>>()?;
            Ok(Some(ParsedEvent::Restake {
                into: d["into"].as_str().context("restake into")?.to_string(),
                from,
                restaked: yocto_str_to_u128(d["restaked"].as_str().context("restake restaked")?)?,
            }))
        }
        "set_feature_enabled" => {
            // ["hash", "increased_score_cap", bool]
            Ok(Some(ParsedEvent::SetIncreasedScoreCap(
                v[2].as_bool().context("set_feature_enabled value")?,
            )))
        }
        "claim" => {
            let items = v[1]["items"].as_array().context("claim items")?;
            let mut total = 0u128;
            for it in items {
                total = total
                    .checked_add(yocto_str_to_u128(it[1].as_str().context("claim item amount")?)?)
                    .context("claim total overflow")?;
            }
            Ok(Some(ParsedEvent::Claim { total }))
        }
        other => bail!("unknown event type {other:?}"),
    }
}

fn str_num_u16(v: &Value) -> Result<u16> {
    match v {
        Value::String(s) => Ok(s.parse()?),
        Value::Number(n) => u16::try_from(n.as_u64().context("not u64")?).context("u16 overflow"),
        _ => bail!("expected string or number, got {v:?}"),
    }
}

fn str_num_u64(v: &Value) -> Result<u64> {
    match v {
        Value::String(s) => Ok(s.parse()?),
        Value::Number(n) => n.as_u64().context("not u64"),
        _ => bail!("expected string or number, got {v:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_score_pairs() {
        let p = parse_event("record_score", None, "[[1345,1787183770765],[2029,1787172973179]]")
            .unwrap()
            .unwrap();
        assert!(matches!(p, ParsedEvent::RecordScore(ref v)
            if v == &vec![(1345u16, 1787183770765u64), (2029, 1787172973179)]));
    }

    #[test]
    fn record_score_empty_is_none() {
        assert!(parse_event("record_score", None, "[]").unwrap().is_none());
    }

    #[test]
    fn apply_booster_applied() {
        let p = parse_event("apply_booster", Some("applied"),
            r#"{"timestamp":"1787025600000","score":"3000"}"#).unwrap().unwrap();
        assert!(matches!(p, ParsedEvent::ApplyBooster { score: 3000, timestamp_ms: 1787025600000 }));
    }

    #[test]
    fn apply_booster_rejected_is_none() {
        assert!(parse_event("apply_booster", Some("rejected"),
            r#"{"timestamp":"1","score":"1"}"#).unwrap().is_none());
    }

    #[test]
    fn deposit_pair() {
        let p = parse_event("deposit", None,
            r#"["hash",["steps_365d_20000_10000_tiered_v1","1000000000000000000"]]"#)
            .unwrap().unwrap();
        assert!(matches!(p, ParsedEvent::Deposit { ref product_id, amount: 1_000_000_000_000_000_000 }
            if product_id == "steps_365d_20000_10000_tiered_v1"));
    }

    #[test]
    fn withdraw_all_list() {
        let p = parse_event("withdraw_all", None,
            r#"["hash",[["365d_12apy","0","50000000000000000000"],["90d_3apy","0","1"]]]"#)
            .unwrap().unwrap();
        assert!(matches!(p, ParsedEvent::WithdrawAll { ref product_ids }
            if product_ids == &vec!["365d_12apy".to_string(), "90d_3apy".to_string()]));
    }

    #[test]
    fn restake_success() {
        let p = parse_event("restake", None,
            r#"["hash",{"from":["a","b"],"into":"365d_12apy","is_success":true,"restaked":"1248350000000000000000","withdrawn":"0","timestamp":1}]"#)
            .unwrap().unwrap();
        assert!(matches!(p, ParsedEvent::Restake { ref into, ref from, restaked: 1_248_350_000_000_000_000_000 }
            if into == "365d_12apy" && from == &vec!["a".to_string(), "b".to_string()]));
    }

    #[test]
    fn restake_failure_is_none() {
        assert!(parse_event("restake", None,
            r#"["hash",{"from":["a"],"into":"a","is_success":false,"restaked":"0","withdrawn":"0","timestamp":1}]"#)
            .unwrap().is_none());
    }

    #[test]
    fn set_feature_enabled_bool() {
        let p = parse_event("set_feature_enabled", None,
            r#"["hash","increased_score_cap",true]"#).unwrap().unwrap();
        assert!(matches!(p, ParsedEvent::SetIncreasedScoreCap(true)));
    }

    #[test]
    fn claim_items_sum() {
        let p = parse_event("claim", None,
            r#"["hash",{"items":[["p1","10"],["p2","5"]],"timestamp":1}]"#).unwrap().unwrap();
        assert!(matches!(p, ParsedEvent::Claim { total: 15 }));
    }

    #[test]
    fn unknown_event_errors() {
        assert!(parse_event("frobnicate", None, "{}").is_err());
    }
}
