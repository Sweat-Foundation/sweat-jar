use std::collections::HashSet;

use anyhow::Result;
use tracing::info;

mod common;
use common::{ft, jar, prepare::prepare_contract, product::RegisterProductCommand};

#[tokio::test]
#[tracing::instrument]
async fn withdraw_all() -> Result<()> {
    const PRINCIPAL: u128 = 1_000_000;
    const JARS_COUNT: u16 = 500;

    common::prepare::init_tracing();
    info!("withdraw all test");

    let product_5_min = RegisterProductCommand::Locked5Minutes60000Percents;
    let product_10_min = RegisterProductCommand::Locked10Minutes60000Percents;

    let context = prepare_contract([product_5_min, product_10_min]).await?;

    let mut product_5_min_total = 0;

    product_5_min_total += PRINCIPAL + 1;
    let amount = jar::create_jar(&context.jar, &context.ft, &context.alice, &product_5_min.id(), PRINCIPAL + 1).await?;
    assert_eq!(amount, PRINCIPAL + 1);

    product_5_min_total += PRINCIPAL + 2;
    jar::create_jar(&context.jar, &context.ft, &context.alice, &product_5_min.id(), PRINCIPAL + 2).await?;

    product_5_min_total += PRINCIPAL * JARS_COUNT as u128;
    context
        .bulk_create_jars(&context.alice, &product_5_min.id(), PRINCIPAL, JARS_COUNT)
        .await?;

    jar::create_jar(&context.jar, &context.ft, &context.alice, &product_10_min.id(), PRINCIPAL + 3).await?;

    context.fast_forward_minutes(6).await?;

    // 2 calls to claim all 502 jars
    jar::claim_total(&context.jar, &context.alice, None).await?;
    jar::claim_total(&context.jar, &context.alice, None).await?;

    let alice_balance = ft::ft_balance_of(&context.ft, context.alice.id()).await?;
    let jar_balance = ft::ft_balance_of(&context.ft, context.jar.id()).await?;

    let withdrawn = jar::withdraw_all(&context.jar, &context.alice, None).await?;
    assert_eq!(withdrawn.withdrawals.len(), 2);

    let alice_balance_after = ft::ft_balance_of(&context.ft, context.alice.id()).await?;
    let jar_balance_after = ft::ft_balance_of(&context.ft, context.jar.id()).await?;

    // The 10-minute jar is immature but fixed jars allow early withdrawal.
    let product_10_min_total = PRINCIPAL + 3;
    let transferred = withdrawn.withdrawn_amount.0 + withdrawn.claimed_amount.0;

    assert_eq!(withdrawn.withdrawn_amount.0, product_5_min_total + product_10_min_total);
    assert_eq!(alice_balance_after - alice_balance, transferred);
    assert_eq!(jar_balance - jar_balance_after, transferred);

    assert_eq!(
        withdrawn
            .withdrawals
            .iter()
            .map(|j| j.withdrawn_amount.0)
            .collect::<HashSet<_>>(),
        HashSet::from([product_5_min_total, product_10_min_total])
    );

    let jars = jar::get_jars_for_account(&context.jar, context.alice.id()).await?;
    assert!(jars.is_empty());

    Ok(())
}
