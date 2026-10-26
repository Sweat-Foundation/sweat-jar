use near_sdk::json_types::Base64VecU8;
use near_workspaces::AccountId;
use serde_json::json;

mod common;
use common::{
    jar,
    panic::PanicFinder,
    prepare::prepare_contract,
    product::RegisterProductCommand,
};

fn insufficient_permissions(method: &str) -> String {
    format!("Insufficient permissions for method {method} restricted by access control.")
}

// --- record_score (Oracle) ---

#[tokio::test]
#[tracing::instrument]
async fn record_score_by_oracle_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    jar::record_score(&context.jar, &context.manager, vec![]).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn record_score_by_non_oracle_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    let result = context
        .alice
        .call(context.jar.id(), "record_score")
        .args_json(json!({ "batch": Vec::<(AccountId, Vec<(u16, u64)>)>::new() }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("record_score")));

    Ok(())
}

// --- apply_booster (Oracle) ---

#[tokio::test]
#[tracing::instrument]
async fn apply_booster_by_oracle_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    jar::apply_booster(&context.jar, &context.manager, vec![], 0, 0).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn apply_booster_by_non_oracle_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    let result = context
        .alice
        .call(context.jar.id(), "apply_booster")
        .args_json(json!({ "account_ids": Vec::<AccountId>::new(), "score": 0u16, "timestamp": 0u64 }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("apply_booster")));

    Ok(())
}

// --- set_timezone (Oracle) ---

#[tokio::test]
#[tracing::instrument]
async fn set_timezone_by_oracle_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    jar::set_timezone(&context.jar, &context.manager, context.alice.id(), 0).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn set_timezone_by_non_oracle_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    let result = context
        .alice
        .call(context.jar.id(), "set_timezone")
        .args_json(json!({ "account_id": context.alice.id(), "timezone": "0" }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("set_timezone")));

    Ok(())
}

// --- unlock_jars_for_account (Maintainer) ---

#[tokio::test]
#[tracing::instrument]
async fn unlock_jars_for_account_by_maintainer_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    jar::create_jar(&context.jar, &context.ft, &context.alice, &product_id, 1_000_000).await?;
    jar::unlock_jars_for_account(&context.jar, &context.manager, context.alice.id(), vec![product_id]).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn unlock_jars_for_account_by_non_maintainer_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    jar::create_jar(&context.jar, &context.ft, &context.alice, &product_id, 1_000_000).await?;

    let result = context
        .bob
        .call(context.jar.id(), "unlock_jars_for_account")
        .args_json(json!({ "account_id": context.alice.id(), "product_ids": [product_id] }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("unlock_jars_for_account")));

    Ok(())
}

// --- set_sunset_at (UpgradeManager) ---

#[tokio::test]
#[tracing::instrument]
async fn set_sunset_at_by_upgrade_manager_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    jar::set_sunset_at(&context.jar, &context.manager, Some(u64::MAX)).await?;
    assert_eq!(Some(u64::MAX), jar::get_sunset_at(&context.jar).await?);

    jar::set_sunset_at(&context.jar, &context.manager, None).await?;
    assert_eq!(None, jar::get_sunset_at(&context.jar).await?);

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn set_sunset_at_by_non_upgrade_manager_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;
    let result = context
        .alice
        .call(context.jar.id(), "set_sunset_at")
        .args_json(json!({ "sunset_at": u64::MAX.to_string() }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("set_sunset_at")));
    assert_eq!(None, jar::get_sunset_at(&context.jar).await?);

    Ok(())
}

// --- set_feature_enabled (Oracle) ---

#[tokio::test]
#[tracing::instrument]
async fn set_feature_enabled_by_oracle_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    jar::set_feature_enabled(&context.jar, &context.manager, context.alice.id(), "increased_apy", true).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn set_feature_enabled_by_non_oracle_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    let result = context
        .alice
        .call(context.jar.id(), "set_feature_enabled")
        .args_json(json!({ "account_id": context.alice.id(), "feature": "increased_apy", "enabled": true }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("set_feature_enabled")));

    Ok(())
}

// --- batch_set_feature_enabled (Oracle) ---

#[tokio::test]
#[tracing::instrument]
async fn batch_set_feature_enabled_by_oracle_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    jar::batch_set_feature_enabled(&context.jar, &context.manager, vec![], "increased_apy", true).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn batch_set_feature_enabled_by_non_oracle_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    let result = context
        .alice
        .call(context.jar.id(), "batch_set_feature_enabled")
        .args_json(json!({ "account_ids": Vec::<AccountId>::new(), "feature": "increased_apy", "enabled": true }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("batch_set_feature_enabled")));

    Ok(())
}

// --- register_product (ProductManager) ---

#[tokio::test]
#[tracing::instrument]
async fn register_product_by_product_manager_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    jar::register_product(
        &context.jar,
        &context.manager,
        RegisterProductCommand::Locked12Months12Percents.get(),
    )
    .await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn register_product_by_non_product_manager_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    let result = context
        .alice
        .call(context.jar.id(), "register_product")
        .args_json(json!({ "product": RegisterProductCommand::Locked12Months12Percents.get() }))
        .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("register_product")));

    Ok(())
}

// --- set_enabled (ProductManager) ---

#[tokio::test]
#[tracing::instrument]
async fn set_enabled_by_product_manager_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    jar::set_enabled(&context.jar, &context.manager, &product_id, false).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn set_enabled_by_non_product_manager_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    let result = context
        .alice
        .call(context.jar.id(), "set_enabled")
        .args_json(json!({ "product_id": product_id, "is_enabled": false }))
        .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("set_enabled")));

    Ok(())
}

// --- set_public_key (ProductManager) ---

#[tokio::test]
#[tracing::instrument]
async fn set_public_key_by_product_manager_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    jar::set_public_key(&context.jar, &context.manager, &product_id, Base64VecU8(vec![1, 2, 3])).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn set_public_key_by_non_product_manager_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    let result = context
        .alice
        .call(context.jar.id(), "set_public_key")
        .args_json(json!({ "product_id": product_id, "public_key": Base64VecU8(vec![1, 2, 3]) }))
        .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("set_public_key")));

    Ok(())
}

// --- withdraw_fee (FeeManager) ---

#[tokio::test]
#[tracing::instrument]
async fn withdraw_fee_by_fee_manager_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked10Minutes6PercentsWithFixedWithdrawFee]).await?;
    let product_id = RegisterProductCommand::Locked10Minutes6PercentsWithFixedWithdrawFee.id();

    jar::create_jar(&context.jar, &context.ft, &context.alice, &product_id, 1_000_000).await?;
    context.fast_forward_hours(1).await?;
    jar::withdraw(&context.jar, &context.alice, &product_id).await?;

    let available_fee = jar::get_fee_amount(&context.jar).await?;
    assert!(available_fee > 0, "test setup should have accrued a nonzero fee");

    jar::withdraw_fee(&context.jar, &context.manager).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn withdraw_fee_by_non_fee_manager_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    let result = context
        .alice
        .call(context.jar.id(), "withdraw_fee")
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("withdraw_fee")));

    Ok(())
}

// --- set_penalty (Oracle, deprecated) ---

#[tokio::test]
#[tracing::instrument]
#[allow(deprecated)]
async fn set_penalty_by_oracle_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    jar::create_jar(&context.jar, &context.ft, &context.alice, &product_id, 1_000_000).await?;
    jar::set_penalty(&context.jar, &context.manager, context.alice.id(), true).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn set_penalty_by_non_oracle_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    let result = context
        .alice
        .call(context.jar.id(), "set_penalty")
        .args_json(json!({ "account_id": context.alice.id(), "value": true }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("set_penalty")));

    Ok(())
}

// --- batch_set_penalty (Oracle, deprecated) ---

#[tokio::test]
#[tracing::instrument]
async fn batch_set_penalty_by_oracle_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    let result = context
        .manager
        .call(context.jar.id(), "batch_set_penalty")
        .args_json(json!({ "account_ids": Vec::<AccountId>::new(), "value": true }))
        .max_gas()
        .transact()
        .await?;

    result.into_result()?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn batch_set_penalty_by_non_oracle_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    let result = context
        .alice
        .call(context.jar.id(), "batch_set_penalty")
        .args_json(json!({ "account_ids": Vec::<AccountId>::new(), "value": true }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("batch_set_penalty")));

    Ok(())
}

// --- bulk_create_jars (Maintainer, integration-test only) ---

#[tokio::test]
#[tracing::instrument]
async fn bulk_create_jars_by_maintainer_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    context.bulk_create_jars(&context.alice, &product_id, 1_000, 1).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn bulk_create_jars_by_non_maintainer_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    let result = context
        .alice
        .call(context.jar.id(), "bulk_create_jars")
        .args_json(json!({
            "account_id": context.alice.id(),
            "product_id": product_id,
            "principal": 1_000u128,
            "number_of_jars": 1u16,
        }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("bulk_create_jars")));

    Ok(())
}

// --- set_time_scale (Maintainer, integration-test only) ---

#[tokio::test]
#[tracing::instrument]
async fn set_time_scale_by_maintainer_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    jar::set_time_scale(&context.jar, &context.manager, 1.0).await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn set_time_scale_by_non_maintainer_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([]).await?;

    let result = context
        .alice
        .call(context.jar.id(), "set_time_scale")
        .args_json(json!({ "time_scale": 1.0 }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("set_time_scale")));

    Ok(())
}

// --- seed_accounts (Maintainer, integration-test only) ---

#[tokio::test]
#[tracing::instrument]
async fn seed_accounts_by_maintainer_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    jar::seed_accounts(
        &context.jar,
        &context.manager,
        &product_id,
        vec![(context.alice.id().clone(), 1_000_000, 0)],
        0,
    )
    .await?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn seed_accounts_by_non_maintainer_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    let result = context
        .alice
        .call(context.jar.id(), "seed_accounts")
        .args_json(json!({
            "product_id": product_id,
            "accounts": [(context.alice.id(), "1000", 0)],
            "deposit_timestamp_ms": 0u64,
        }))
        .max_gas()
        .transact()
        .await?;

    assert!(result.into_result().has_panic(&insufficient_permissions("seed_accounts")));

    Ok(())
}

// --- airdrop (Oracle, via ft_on_transfer's FtMessage::Airdrop — not an
// #[access_control_any]-attributed method, so it panics with a custom
// require! message, not the ACL macro's fixed message format) ---

#[tokio::test]
#[tracing::instrument]
async fn airdrop_by_oracle_succeeds() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    let msg = json!({
        "type": "airdrop",
        "data": {
            "ticket": { "product_id": product_id, "valid_until": "0" },
            "receivers": [context.alice.id(), context.bob.id()],
        }
    });

    context
        .manager
        .call(context.ft.id(), "ft_transfer_call")
        .args_json(json!({
            "receiver_id": context.jar.id(),
            "amount": "2000000",
            "msg": msg.to_string(),
        }))
        .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    Ok(())
}

#[tokio::test]
#[tracing::instrument]
async fn airdrop_by_non_oracle_panics() -> anyhow::Result<()> {
    common::prepare::init_tracing();
    let context = prepare_contract([RegisterProductCommand::Locked12Months12Percents]).await?;
    let product_id = RegisterProductCommand::Locked12Months12Percents.id();

    let msg = json!({
        "type": "airdrop",
        "data": {
            "ticket": { "product_id": product_id, "valid_until": "0" },
            "receivers": [context.alice.id(), context.bob.id()],
        }
    });

    let result = context
        .alice
        .call(context.ft.id(), "ft_transfer_call")
        .args_json(json!({
            "receiver_id": context.jar.id(),
            "amount": "2000000",
            "msg": msg.to_string(),
        }))
        .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?;

    assert!(result
        .into_result()
        .has_panic("Only accounts with the Oracle role can perform airdrops"));

    Ok(())
}
