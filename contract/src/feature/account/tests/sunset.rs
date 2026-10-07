use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::{json_types::U128, serde_json::json, AccountId};
use rstest::rstest;
use sweat_jar_model::{
    api::{AccountApi, ClaimApi, RestakeApi, WithdrawApi},
    data::{deposit::DepositTicket, jar::Jar, product::Product},
    start_of_the_day, Timezone, TokenAmount, MS_IN_DAY, MS_IN_HOUR, SUNSET_AT,
};

use crate::{
    common::{
        env::test_env_ext,
        testing::{
            accounts::{admin, alice},
            expect_panic, Context,
        },
    },
    feature::{
        account::model::test_utils::jar,
        product::model::test_utils::{product_1_year_12_percent, product_steps_365d_20000_score_cap},
    },
};

fn total_interest(context: &Context, account_id: &AccountId) -> TokenAmount {
    context.contract().get_total_interest(account_id.clone()).amount.total.0
}

#[rstest]
fn fixed_jar_interest_stops_at_sunset(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] product: Product,
    #[with(vec![(SUNSET_AT - 100 * MS_IN_DAY, 1_000_000_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_ms(SUNSET_AT - MS_IN_DAY);
    let interest_before_sunset = total_interest(&context, &alice);

    context.set_block_timestamp_in_ms(SUNSET_AT);
    let interest_at_sunset = total_interest(&context, &alice);
    assert!(interest_at_sunset > interest_before_sunset);

    context.set_block_timestamp_in_ms(SUNSET_AT + 30 * MS_IN_DAY);
    assert_eq!(interest_at_sunset, total_interest(&context, &alice));
}

#[rstest]
fn step_jar_interest_stops_at_sunset(
    admin: AccountId,
    alice: AccountId,
    #[from(product_steps_365d_20000_score_cap)] product: Product,
) {
    test_env_ext::set_test_log_events(false);

    let mut context = Context::new(admin).with_products(&[product.clone()]);

    let first_day = start_of_the_day(SUNSET_AT) - 5 * MS_IN_DAY;
    context.set_block_timestamp_in_ms(first_day);

    {
        let mut contract = context.contract();
        let account = contract.get_or_create_account_mut(&alice);
        account.try_set_timezone(Timezone::hour_shift(0).into());
        account.deposit(&product.id, 365_000_000_000_000_000_000, first_day.into());
    }

    let record_daily_scores = |context: &mut Context, days: std::ops::Range<u64>| {
        for day in days {
            let now = first_day + day * MS_IN_DAY + 12 * MS_IN_HOUR;
            context.set_block_timestamp_in_ms(now);
            context.record_score(&alice, now.into(), 10_000);
        }
    };

    // The last score before the sunset is recorded on its day, before 14:00 UTC.
    record_daily_scores(&mut context, 0..6);

    context.set_block_timestamp_in_ms(SUNSET_AT);
    let interest_at_sunset = total_interest(&context, &alice);
    assert_ne!(0, interest_at_sunset);

    // Scores recorded after the sunset don't add interest.
    record_daily_scores(&mut context, 6..10);
    assert_eq!(interest_at_sunset, total_interest(&context, &alice));
}

#[rstest]
fn user_operations_are_allowed_right_before_sunset(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] product: Product,
    #[with(vec![(SUNSET_AT - 100 * MS_IN_DAY, 1_000_000_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_ms(SUNSET_AT - 1);
    assert_ne!(0, context.claim_total(&alice));
}

#[rstest]
fn user_operations_are_rejected_after_sunset(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] product: Product,
    #[with(vec![(SUNSET_AT - 400 * MS_IN_DAY, 1_000_000_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_ms(SUNSET_AT);
    context.switch_account(&alice);

    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: (SUNSET_AT + MS_IN_DAY).into(),
        timezone: None,
    };

    expect_panic(&context, "Jars are closed", || {
        context.contract().claim_total(None);
    });
    expect_panic(&context, "Jars are closed", || {
        context.contract().withdraw(product.id.clone());
    });
    expect_panic(&context, "Jars are closed", || {
        context.contract().withdraw_all(None);
    });
    expect_panic(&context, "Jars are closed", || {
        context
            .contract()
            .restake(product.id.clone(), ticket.clone(), None, None);
    });
    expect_panic(&context, "Jars are closed", || {
        context.contract().restake_all(ticket.clone(), None, None);
    });

    context.switch_account_to_ft_contract_account();
    let message = json!({
        "type": "stake",
        "data": { "ticket": ticket },
    });
    expect_panic(&context, "Jars are closed", || {
        context
            .contract()
            .ft_on_transfer(alice.clone(), U128(1_000_000), message.to_string());
    });

    let jar = context.contract().get_account(&alice).get_jar(&product.id).clone();
    assert_eq!(1_000_000_000, jar.total_principal());
    assert!(!jar.is_locked);
}
