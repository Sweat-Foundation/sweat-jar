//! Lightweight gas-measurement tests.
//!
//! These are `#[ignore]`d so a plain `cargo test`/CI run skips them; run them
//! explicitly via `make measure-gas`. Their printed TGas figures are what
//! `contract/src/feature/claim/api.rs`'s `INITIAL_GAS_FOR_AFTER_CLAIM` /
//! `ADDITIONAL_AFTER_CLAIM_JAR_COST` and `contract/src/feature/withdraw/api.rs`'s
//! `GAS_FOR_AFTER_WITHDRAW` / `GAS_FOR_BULK_AFTER_WITHDRAW` are tuned from —
//! re-run after touching the `after_claim`/`after_withdraw` callback logic and
//! re-check those constants still leave enough headroom.

use anyhow::Result;
use near_workspaces::types::Gas;

mod common;
use common::{jar, prepare::prepare_contract, product::RegisterProductCommand};

fn report(label: &str, samples: &[(usize, Gas)]) {
    println!("\n=== {label} ===");
    for &(count, gas) in samples {
        println!("{count:>5} jars: {:>8.3} TGas", gas.as_gas() as f64 / 1e12);
    }
    for pair in samples.windows(2) {
        let (count_a, gas_a) = pair[0];
        let (count_b, gas_b) = pair[1];
        let diff = gas_b.as_gas() as i128 - gas_a.as_gas() as i128;
        let per_step = diff as f64 / (count_b - count_a) as f64 / 1e9;
        println!("  {count_a:>5} -> {count_b:>5}: {:+.3} TGas total, {per_step:+.3} GGas/step", diff as f64 / 1e12);
    }
}

const AFTER_CLAIM_JAR_COUNTS: [usize; 4] = [1, 50, 100, 200];

/// Measures `claim_total`'s gas cost (dominated by the `after_claim` callback)
/// as a function of the number of jars claimed at once.
#[tokio::test]
#[ignore = "run explicitly via `make measure-gas`"]
async fn measure_after_claim_gas() -> Result<()> {
    let product = RegisterProductCommand::Locked6Months6PercentsWithWithdrawFee;
    let product_id = product.id();
    let mut samples = Vec::new();

    for &jars_count in &AFTER_CLAIM_JAR_COUNTS {
        let context = prepare_contract([product]).await?;

        for _ in 0..jars_count {
            jar::create_jar(&context.jar, &context.ft, &context.alice, &product_id, 100_000).await?;
        }

        context.fast_forward_hours(2).await?;

        let outcome = jar::claim_total_raw(&context.jar, &context.alice, None).await?;
        let gas = outcome.total_gas_burnt;
        outcome.into_result()?;

        samples.push((jars_count, gas));
    }

    report("claim_total (after_claim callback)", &samples);

    Ok(())
}

const WITHDRAW_PRINCIPALS: [u128; 3] = [100_000, 300_000, 500_000];

/// Measures `withdraw`'s gas cost (dominated by the `after_withdraw` callback)
/// for a single jar with a withdrawal fee, across a range of principals.
#[tokio::test]
#[ignore = "run explicitly via `make measure-gas`"]
async fn measure_after_withdraw_gas() -> Result<()> {
    let product = RegisterProductCommand::Locked10Minutes6PercentsWithPercentWithdrawFee;
    let product_id = product.id();
    let mut samples = Vec::new();

    for &principal in &WITHDRAW_PRINCIPALS {
        let context = prepare_contract([product]).await?;

        jar::create_jar(&context.jar, &context.ft, &context.alice, &product_id, principal).await?;
        context.fast_forward_hours(1).await?;

        let outcome = jar::withdraw_raw(&context.jar, &context.alice, &product_id).await?;
        let gas = outcome.total_gas_burnt;
        outcome.into_result()?;

        samples.push((principal as usize, gas));
    }

    report("withdraw (after_withdraw callback)", &samples);

    Ok(())
}

const BULK_WITHDRAW_JAR_COUNTS: [u16; 3] = [1, 100, 200];

/// Measures `withdraw_all`'s gas cost (dominated by the bulk `after_withdraw`
/// callback) as a function of the number of jars withdrawn at once.
#[tokio::test]
#[ignore = "run explicitly via `make measure-gas`"]
async fn measure_bulk_withdraw_gas() -> Result<()> {
    let product = RegisterProductCommand::Locked5Minutes60000Percents;
    let product_id = product.id();
    let mut samples = Vec::new();

    for &jars_count in &BULK_WITHDRAW_JAR_COUNTS {
        let context = prepare_contract([product]).await?;

        context.bulk_create_jars(&context.alice, &product_id, 10_000, jars_count).await?;
        context.fast_forward_minutes(6).await?;
        jar::claim_total(&context.jar, &context.alice, None).await?;

        let outcome = jar::withdraw_all_raw(&context.jar, &context.alice, None).await?;
        let gas = outcome.total_gas_burnt;
        outcome.into_result()?;

        samples.push((jars_count as usize, gas));
    }

    report("withdraw_all (bulk after_withdraw callback)", &samples);

    Ok(())
}

const RESTAKE_REMAINDER_PRINCIPALS: [u128; 3] = [200_000, 600_000, 1_000_000];

/// Measures `restake`'s gas cost when a non-zero withdrawal remainder is
/// produced (dominated by the `after_transfer_remainder` callback), across a
/// range of principals. Restaking less than the full balance always produces
/// a remainder.
#[tokio::test]
#[ignore = "run explicitly via `make measure-gas`"]
async fn measure_after_restake_remainder_gas() -> Result<()> {
    let source = RegisterProductCommand::Flexible6Months6Percents;
    let target = RegisterProductCommand::Locked10Minutes20000ScoreCap;
    let source_id = source.id();
    let target_id = target.id();
    let mut samples = Vec::new();

    for &principal in &RESTAKE_REMAINDER_PRINCIPALS {
        let context = prepare_contract([source]).await?;
        let signer = jar::register_protected_product(&context.jar, &context.manager, target.get()).await?;

        jar::create_jar(&context.jar, &context.ft, &context.alice, &source_id, principal).await?;

        let (ticket, signature) =
            jar::signed_restake_ticket(&context.jar, &context.alice, &target_id, &signer, principal / 2, 1).await?;
        let outcome = jar::restake_raw(
            &context.jar,
            &context.alice,
            &source_id,
            ticket,
            Some(signature),
            Some((principal / 2).into()),
        )
        .await?;
        let gas = outcome.total_gas_burnt;
        outcome.into_result()?;

        samples.push((principal as usize, gas));
    }

    report("restake with remainder (after_transfer_remainder callback)", &samples);

    Ok(())
}
