use serde_json::{json, Value};
use sweat_jar_model::data::withdraw::WithdrawView;
use tracing::info;

mod common;
use common::{ft, jar, prepare::prepare_contract, product::RegisterProductCommand};

#[tokio::test]
#[tracing::instrument]
async fn withdraw_fixed_jar_before_maturity_with_interest() -> anyhow::Result<()> {
    const PRINCIPAL: u128 = 1_000_000;

    common::prepare::init_tracing();
    info!("withdraw fixed jar before maturity with interest test");

    let product = RegisterProductCommand::Locked10Minutes60000Percents;
    let context = prepare_contract([product]).await?;

    jar::create_jar(&context.jar, &context.ft, &context.alice, &product.id(), PRINCIPAL).await?;

    // Lockup term is 10 minutes, so the jar is still immature.
    context.fast_forward_minutes(3).await?;

    let interest_before = jar::get_total_interest(&context.jar, context.alice.id())
        .await?
        .amount
        .total
        .0;
    assert_ne!(0, interest_before);

    let alice_balance = ft::ft_balance_of(&context.ft, context.alice.id()).await?;

    let outcome = jar::withdraw_raw(&context.jar, &context.alice, &product.id())
        .await?
        .into_result()?;
    let withdrawn: WithdrawView = outcome.json()?;

    assert_eq!(PRINCIPAL, withdrawn.withdrawn_amount.0);
    assert!(withdrawn.claimed_amount.0 >= interest_before);

    let alice_balance_after = ft::ft_balance_of(&context.ft, context.alice.id()).await?;
    assert_eq!(
        withdrawn.withdrawn_amount.0 + withdrawn.claimed_amount.0,
        alice_balance_after - alice_balance
    );

    // Interest is reported as a regular claim alongside the withdrawal.
    let events: Vec<Value> = outcome
        .logs()
        .iter()
        .filter_map(|log| log.strip_prefix("EVENT_JSON:"))
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let event = |name: &str| events.iter().find(|event| event["event"] == name).cloned();

    assert!(event("withdraw").is_some());
    let claim = event("claim").expect("Claim event is missing");
    assert_eq!(
        claim["data"][1]["items"][0][1],
        json!(withdrawn.claimed_amount.0.to_string())
    );

    assert!(jar::get_jars_for_account(&context.jar, context.alice.id())
        .await?
        .is_empty());
    assert_eq!(
        0,
        jar::get_total_interest(&context.jar, context.alice.id())
            .await?
            .amount
            .total
            .0
    );

    Ok(())
}
