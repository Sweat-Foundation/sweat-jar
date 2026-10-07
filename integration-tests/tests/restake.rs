use anyhow::Result;
use tracing::info;

mod common;
use common::{jar, prepare::prepare_contract, product::RegisterProductCommand};

#[tokio::test]
#[tracing::instrument]
async fn restake() -> Result<()> {
    common::prepare::init_tracing();
    info!("restake test");

    let product = RegisterProductCommand::Locked10Minutes6Percents;
    let target = RegisterProductCommand::Locked10Minutes20000ScoreCap;
    let context = prepare_contract([product]).await?;
    let signer = jar::register_protected_product(&context.jar, &context.manager, target.get()).await?;

    let amount = 1_000_000;
    jar::create_jar(&context.jar, &context.ft, &context.alice, &product.id(), amount).await?;

    let jars = jar::get_jars_for_account(&context.jar, context.alice.id()).await?;
    assert_eq!(1, jars.get_total_deposits_number());
    assert_eq!(amount, jars.get_total_principal());

    let first_jar_timestamp = jars.0.get(&product.id()).unwrap().first().unwrap().0;

    // Not mature yet: restake into a score-based product takes immature deposits too.
    context.fast_forward_minutes(1).await?;
    let (ticket, signature) =
        jar::signed_restake_ticket(&context.jar, &context.alice, &target.id(), &signer, amount, 1).await?;
    jar::restake(
        &context.jar,
        &context.alice,
        &product.id(),
        ticket,
        Some(signature),
        None,
    )
    .await?;

    let jars = jar::get_jars_for_account(&context.jar, context.alice.id()).await?;
    assert_eq!(1, jars.get_total_deposits_number());
    assert_eq!(amount, jars.get_total_principal());

    let second_jar_timestamp = jars.0.get(&target.id()).unwrap().first().unwrap().0;
    assert!(second_jar_timestamp > first_jar_timestamp);

    jar::claim_total(&context.jar, &context.alice, None).await?;

    let jars = jar::get_jars_for_account(&context.jar, context.alice.id()).await?;
    assert_eq!(jars.get_total_deposits_number(), 1);

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn restake_all() -> Result<()> {
    const PRINCIPAL: u128 = 1_000_000;
    const JARS_COUNT: u16 = 5010;

    common::prepare::init_tracing();
    info!("restake all test");

    let product_5_min = RegisterProductCommand::Locked5Minutes60000Percents;
    let product_10_min = RegisterProductCommand::Locked10Minutes60000Percents;
    let target = RegisterProductCommand::Locked10Minutes20000ScoreCap;

    let context = prepare_contract([product_5_min, product_10_min]).await?;
    let signer = jar::register_protected_product(&context.jar, &context.manager, target.get()).await?;

    let amount = jar::create_jar(
        &context.jar,
        &context.ft,
        &context.alice,
        &product_5_min.id(),
        PRINCIPAL + 1,
    )
    .await?;
    assert_eq!(amount, PRINCIPAL + 1);

    jar::create_jar(
        &context.jar,
        &context.ft,
        &context.alice,
        &product_5_min.id(),
        PRINCIPAL + 2,
    )
    .await?;
    jar::create_jar(
        &context.jar,
        &context.ft,
        &context.alice,
        &product_10_min.id(),
        PRINCIPAL + 3,
    )
    .await?;

    context
        .bulk_create_jars(&context.alice, &product_5_min.id(), PRINCIPAL, JARS_COUNT)
        .await?;

    let total = 3 * PRINCIPAL + 6 + JARS_COUNT as u128 * PRINCIPAL;

    // The 5-minute jars are mature, the 10-minute one is not: all of them are restaked.
    context.fast_forward_minutes(6).await?;

    jar::claim_total(&context.jar, &context.alice, None).await?;

    // Three regular deposits so far, bulk-created jars don't bump the nonce.
    let (ticket, signature) =
        jar::signed_restake_ticket(&context.jar, &context.alice, &target.id(), &signer, total, 3).await?;
    jar::restake_all(&context.jar, &context.alice, ticket, Some(signature), None).await?;

    let jars = jar::get_jars_for_account(&context.jar, context.alice.id()).await?;
    assert_eq!(1, jars.get_total_deposits_number());
    assert_eq!(total, jars.get_total_principal_for_product(&target.id()));

    Ok(())
}
