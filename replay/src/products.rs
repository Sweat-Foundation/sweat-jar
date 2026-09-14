//! Fetch and load the on-chain product catalogue for the replay engine.

use std::path::Path;

use anyhow::{anyhow, Context};
use serde_json::{json, Value};
use sweat_jar_model::data::product::Product;

pub const MAINNET_RPC: &str = "https://rpc.mainnet.near.org";
pub const JAR_CONTRACT: &str = "v2.jars.sweat";

/// POSTs `get_products()` to the RPC, decodes the byte-array result, re-serializes
/// it pretty, and writes it to `out`. Returns the number of products written.
pub fn fetch_products(rpc_url: &str, contract: &str, out: &Path) -> anyhow::Result<usize> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": "1",
        "method": "query",
        "params": {
            "request_type": "call_function",
            "finality": "final",
            "account_id": contract,
            "method_name": "get_products",
            "args_base64": "e30=",
        }
    });

    let response = ureq::post(rpc_url)
        .send_json(body)
        .with_context(|| format!("POST get_products to {rpc_url}"))?;

    if response.status() != 200 {
        return Err(anyhow!("RPC returned HTTP {}", response.status()));
    }

    let value: Value = response.into_json().context("decode RPC response as JSON")?;

    let bytes_json = value
        .get("result")
        .and_then(|r| r.get("result"))
        .ok_or_else(|| anyhow!("RPC response missing result.result: {value}"))?;

    let bytes: Vec<u8> =
        serde_json::from_value(bytes_json.clone()).context("result.result is not a byte array")?;

    let decoded = String::from_utf8(bytes).context("result.result bytes are not valid UTF-8")?;

    let products: Vec<Product> =
        serde_json::from_str(&decoded).context("parse get_products payload into Vec<Product>")?;

    let pretty = serde_json::to_string_pretty(&products).context("re-serialize products")?;
    std::fs::write(out, pretty).with_context(|| format!("write {}", out.display()))?;

    Ok(products.len())
}

/// Parses the file written by [`fetch_products`] (a JSON array) into engine products.
pub fn load_products(path: &Path) -> anyhow::Result<Vec<Product>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let products: Vec<Product> = serde_json::from_str(&text)
        .with_context(|| format!("parse {} into Vec<Product>", path.display()))?;
    // The replay submits unsigned deposits; signature verification is out of scope.
    Ok(products
        .into_iter()
        .map(|p| Product {
            public_key: None,
            ..p
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_products_parses_the_live_shape() {
        let products =
            load_products(std::path::Path::new("tests/fixtures/products.json")).unwrap();
        assert!(products.len() >= 20);
        assert!(products.iter().all(|p| p.public_key.is_none()));

        let p = products
            .iter()
            .find(|p| p.id == "365d_12apy")
            .expect("365d_12apy present");
        assert!(matches!(
            p.terms,
            sweat_jar_model::data::product::Terms::Fixed(_)
        ));

        assert!(products.iter().any(|p| matches!(
            p.terms,
            sweat_jar_model::data::product::Terms::ScoreBased(_)
                | sweat_jar_model::data::product::Terms::TieredScoreBased(_)
        )));
    }
}
