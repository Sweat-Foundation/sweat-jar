use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::{json_types::U128, serde_json::json, AccountId};
use rstest::rstest;
use sweat_jar_model::{
    api::{AccountApi, ClaimApi, RestakeApi, SunsetApi, WithdrawApi},
    data::{deposit::DepositTicket, jar::Jar, product::Product},
    start_of_the_day, Timezone, TokenAmount, MS_IN_DAY, MS_IN_HOUR,
};

use crate::{
    common::{
        env::test_env_ext,
        testing::{
            accounts::{admin, alice},
            expect_panic, Context, SUNSET_AT,
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
        .with_sunset(SUNSET_AT)
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

    let mut context = Context::new(admin)
        .with_sunset(SUNSET_AT)
        .with_products(&[product.clone()]);

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
        .with_sunset(SUNSET_AT)
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
        .with_sunset(SUNSET_AT)
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

#[rstest]
fn jars_never_close_by_default(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] product: Product,
    #[with(vec![(SUNSET_AT - 100 * MS_IN_DAY, 1_000_000_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    assert_eq!(None, context.contract().get_sunset_at());

    context.set_block_timestamp_in_ms(SUNSET_AT);
    let interest_at_reference_sunset = total_interest(&context, &alice);

    context.set_block_timestamp_in_ms(SUNSET_AT + 30 * MS_IN_DAY);
    assert!(total_interest(&context, &alice) > interest_at_reference_sunset);
    assert_ne!(0, context.claim_total(&alice));
}

#[rstest]
fn removed_sunset_keeps_interest_and_operations_running(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] product: Product,
    #[with(vec![(SUNSET_AT - 100 * MS_IN_DAY, 1_000_000_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_sunset(SUNSET_AT)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.switch_account_to_operator();
    context.contract().set_sunset_at(None);
    assert_eq!(None, context.contract().get_sunset_at());

    context.set_block_timestamp_in_ms(SUNSET_AT);
    let interest_at_default_sunset = total_interest(&context, &alice);

    context.set_block_timestamp_in_ms(SUNSET_AT + 30 * MS_IN_DAY);
    assert!(total_interest(&context, &alice) > interest_at_default_sunset);

    assert_ne!(0, context.claim_total(&alice));

    context.switch_account_to_ft_contract_account();
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: (SUNSET_AT + 31 * MS_IN_DAY).into(),
        timezone: None,
    };
    let message = json!({ "type": "stake", "data": { "ticket": ticket } });
    context
        .contract()
        .ft_on_transfer(alice.clone(), U128(1_000_000), message.to_string());

    let jar = context.contract().get_account(&alice).get_jar(&product.id).clone();
    assert_eq!(1_001_000_000, jar.total_principal());
}

#[rstest]
fn postponed_sunset_moves_interest_cutoff_and_closing(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] product: Product,
    #[with(vec![(SUNSET_AT - 100 * MS_IN_DAY, 1_000_000_000)])] jar: Jar,
) {
    let new_sunset = SUNSET_AT + 100 * MS_IN_DAY;

    let mut context = Context::new(admin)
        .with_sunset(SUNSET_AT)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.switch_account_to_operator();
    context.contract().set_sunset_at(Some(new_sunset.into()));

    context.set_block_timestamp_in_ms(SUNSET_AT);
    let interest_at_default_sunset = total_interest(&context, &alice);

    context.set_block_timestamp_in_ms(new_sunset);
    let interest_at_new_sunset = total_interest(&context, &alice);
    assert!(interest_at_new_sunset > interest_at_default_sunset);

    context.set_block_timestamp_in_ms(new_sunset + 30 * MS_IN_DAY);
    assert_eq!(interest_at_new_sunset, total_interest(&context, &alice));

    context.switch_account(&alice);
    expect_panic(&context, "Jars are closed", || {
        context.contract().claim_total(None);
    });
}

#[rstest]
fn removing_sunset_after_it_passed_reopens_jars(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] product: Product,
    #[with(vec![(SUNSET_AT - 400 * MS_IN_DAY, 1_000_000_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_sunset(SUNSET_AT)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_ms(SUNSET_AT + MS_IN_DAY);
    context.switch_account_to_operator();
    context.contract().set_sunset_at(None);

    context.switch_account(&alice);
    context.contract().withdraw(product.id.clone());
}

#[rstest]
fn sunset_in_the_past_is_rejected(admin: AccountId) {
    let mut context = Context::new(admin).with_sunset(SUNSET_AT);
    context.set_block_timestamp_in_ms(SUNSET_AT - MS_IN_DAY);
    context.switch_account_to_operator();

    expect_panic(&context, "Sunset must be in the future", || {
        context.contract().set_sunset_at(Some((SUNSET_AT - MS_IN_DAY).into()));
    });
    assert_eq!(Some(SUNSET_AT.into()), context.contract().get_sunset_at());
}

#[rstest]
#[should_panic(expected = "Insufficient permissions")]
fn sunset_is_set_only_by_upgrade_manager(admin: AccountId, alice: AccountId) {
    let mut context = Context::new(admin).with_sunset(SUNSET_AT);
    context.switch_account(&alice);

    context.contract().set_sunset_at(None);
}

#[rstest]
fn step_jar_interest_continues_without_sunset(
    admin: AccountId,
    alice: AccountId,
    #[from(product_steps_365d_20000_score_cap)] product: Product,
) {
    test_env_ext::set_test_log_events(false);

    let mut context = Context::new(admin)
        .with_sunset(SUNSET_AT)
        .with_products(&[product.clone()]);
    context.switch_account_to_operator();
    context.contract().set_sunset_at(None);

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

    // Equal windows before and after the default sunset earn the same interest.
    record_daily_scores(&mut context, 0..5);
    let interest_before = total_interest(&context, &alice);
    record_daily_scores(&mut context, 5..9);
    let interest_around = total_interest(&context, &alice);
    record_daily_scores(&mut context, 9..13);
    let interest_after = total_interest(&context, &alice);

    assert_eq!(interest_around - interest_before, interest_after - interest_around);
}
