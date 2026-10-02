use tracing::info;

mod common;
use common::{ft, jar, prepare::prepare_contract, product::RegisterProductCommand};

#[tokio::test]
#[tracing::instrument]
async fn test_fixed_withdraw_fee() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    info!("fixed withdraw fee test");

    let context = prepare_contract([RegisterProductCommand::Locked10Minutes6PercentsWithFixedWithdrawFee]).await?;

    let fee_balance_before = ft::ft_balance_of(&context.ft, context.fee.id()).await?;

    let product_id = RegisterProductCommand::Locked10Minutes6PercentsWithFixedWithdrawFee.id();
    jar::create_jar(&context.jar, &context.ft, &context.alice, &product_id, 1_000_000).await?;

    let mut alice_balance = ft::ft_balance_of(&context.ft, context.alice.id()).await?;
    assert_eq!(99_999_999_999_999_999_999_000_000, alice_balance);

    context.fast_forward_hours(1).await?;

    let withdraw_result = jar::withdraw(&context.jar, &context.alice, &product_id).await?;
    assert_eq!(999_000, withdraw_result.withdrawn_amount.0);
    assert_eq!(1_000, withdraw_result.fee.0);

    alice_balance = ft::ft_balance_of(&context.ft, context.alice.id()).await?;
    // Fixed jars pay out accrued interest along with the principal.
    assert_eq!(99_999_999_999_999_999_999_999_000 + withdraw_result.interest.0, alice_balance);

    let expected_fee = 1_000;
    let available_fee = jar::get_fee_amount(&context.jar).await?;
    assert_eq!(expected_fee, available_fee);

    let withdrawn_fee = jar::withdraw_fee(&context.jar, &context.manager).await?;
    assert_eq!(expected_fee, withdrawn_fee);

    let fee_balance_after = ft::ft_balance_of(&context.ft, context.fee.id()).await?;
    assert_eq!(expected_fee, fee_balance_after - fee_balance_before);

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn test_percent_withdraw_fee() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    info!("percent withdraw fee test");

    let context = prepare_contract([RegisterProductCommand::Locked10Minutes6PercentsWithPercentWithdrawFee]).await?;

    let fee_balance_before = ft::ft_balance_of(&context.ft, context.fee.id()).await?;

    let product_id = RegisterProductCommand::Locked10Minutes6PercentsWithPercentWithdrawFee.id();
    jar::create_jar(&context.jar, &context.ft, &context.alice, &product_id, 1_000_000).await?;

    let mut alice_balance = ft::ft_balance_of(&context.ft, context.alice.id()).await?;
    assert_eq!(99_999_999_999_999_999_999_000_000, alice_balance);

    context.fast_forward_hours(1).await?;

    let withdraw_result = jar::withdraw(&context.jar, &context.alice, &product_id).await?;
    assert_eq!(990_000, withdraw_result.withdrawn_amount.0);
    assert_eq!(10_000, withdraw_result.fee.0);

    alice_balance = ft::ft_balance_of(&context.ft, context.alice.id()).await?;
    // Fixed jars pay out accrued interest along with the principal.
    assert_eq!(99_999_999_999_999_999_999_990_000 + withdraw_result.interest.0, alice_balance);

    let expected_fee = 10_000;
    let available_fee = jar::get_fee_amount(&context.jar).await?;
    assert_eq!(expected_fee, available_fee);

    let withdrawn_fee = jar::withdraw_fee(&context.jar, &context.manager).await?;
    assert_eq!(expected_fee, withdrawn_fee);

    let fee_balance_after = ft::ft_balance_of(&context.ft, context.fee.id()).await?;
    assert_eq!(expected_fee, fee_balance_after - fee_balance_before);

    Ok(())
}
