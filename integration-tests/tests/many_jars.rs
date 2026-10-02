use anyhow::Result;
use sweat_jar_model::data::{claim::ClaimedAmountView, deposit::DepositTicket};
use tracing::info;

mod common;
use common::{jar, prepare::prepare_contract, product::RegisterProductCommand::Locked5Minutes60000Percents};

#[tokio::test]
#[tracing::instrument]
async fn claim_many_jars() -> Result<()> {
    const DEPOSIT_PRINCIPAL: u128 = 1_000;
    const DEPOSITS_COUNT: usize = 15_000;

    common::prepare::init_tracing();
    info!("claim many jars test");

    let context = prepare_contract([Locked5Minutes60000Percents]).await?;

    context
        .bulk_create_jars(
            &context.alice,
            &Locked5Minutes60000Percents.id(),
            DEPOSIT_PRINCIPAL,
            DEPOSITS_COUNT as u16,
        )
        .await?;

    assert_eq!(
        jar::get_jars_for_account(&context.jar, context.alice.id())
            .await?
            .get_total_deposits_number(),
        DEPOSITS_COUNT
    );

    context.fast_forward_minutes(10).await?;

    let claimed = jar::claim_total(&context.jar, &context.alice, Some(true)).await?;
    let batch_claim_sum = claimed.get_total().0;
    assert_ne!(0, batch_claim_sum);

    assert_eq!(
        DEPOSITS_COUNT,
        jar::get_jars_for_account(&context.jar, context.alice.id())
            .await?
            .get_total_deposits_number(),
    );

    let withdrawn = jar::withdraw_all(&context.jar, &context.alice, None).await?;
    assert_eq!(1, withdrawn.withdrawals.len());
    assert_eq!(DEPOSITS_COUNT as u128 * DEPOSIT_PRINCIPAL, withdrawn.total_amount.0);

    assert_eq!(
        jar::get_jars_for_account(&context.jar, context.alice.id())
            .await?
            .get_total_deposits_number(),
        0
    );

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn restake_many_jars() -> Result<()> {
    const DEPOSIT_PRINCIPAL: u128 = 5 * 10u128.pow(18);
    const DEPOSITS_COUNT: usize = 15_000;

    common::prepare::init_tracing();
    info!("restake many jars test");

    let context = prepare_contract([Locked5Minutes60000Percents]).await?;
    common::ft::tge_mint(&context.ft, context.jar.id(), 100_000_000 * 10u128.pow(18)).await?;

    let product_id = Locked5Minutes60000Percents.id();
    context
        .bulk_create_jars(&context.alice, &product_id, DEPOSIT_PRINCIPAL, DEPOSITS_COUNT as u16)
        .await?;

    let original_jars = jar::get_jars_for_account(&context.jar, context.alice.id()).await?;
    assert_eq!(original_jars.get_total_deposits_number(), DEPOSITS_COUNT);

    let mut original_dates: Vec<u64> = original_jars
        .0
        .values()
        .flat_map(|deposits| deposits.iter().map(|(timestamp, _)| timestamp.0))
        .collect();
    original_dates.sort_unstable();
    let original_date_latest = *original_dates.last().unwrap();

    context.fast_forward_minutes(10).await?;

    let ClaimedAmountView::Detailed(claimed) = jar::claim_total(&context.jar, &context.alice, Some(true)).await? else {
        panic!();
    };
    assert_eq!(1, claimed.detailed.len());

    let ticket = DepositTicket {
        product_id: product_id.clone(),
        valid_until: 0.into(),
        timezone: None,
    };
    jar::restake_all(&context.jar, &context.alice, ticket, None, None).await?;

    let restaked_jars = jar::get_jars_for_account(&context.jar, context.alice.id()).await?;
    assert_eq!(1, restaked_jars.get_total_deposits_number());
    let restake_date = restaked_jars.0.get(&product_id).unwrap().first().unwrap().0;

    assert!(original_date_latest < restake_date.0);

    Ok(())
}
