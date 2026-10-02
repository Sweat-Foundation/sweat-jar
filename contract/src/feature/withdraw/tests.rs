#![cfg(test)]

use std::collections::HashSet;

use near_sdk::{json_types::U64, AccountId, PromiseOrValue};
use rstest::{fixture, rstest};
use sweat_jar_model::{
    api::{AccountApi, ClaimApi, FeeApi, WithdrawApi},
    data::{
        jar::*,
        product::{Apy, Cap, FixedProductTerms, Product, ProductId, Terms},
        withdraw::BulkWithdrawView,
    },
    Timezone, TokenAmount, MS_IN_DAY,
};
use sweat_jar_primitives::UDecimal;

use crate::{
    common::{
        env::test_env_ext,
        event::EventKind,
        testing::{accounts::*, expect_panic, Context, TokenUtils, UnwrapPromise},
    },
    feature::{
        account::model::test_utils::jar,
        product::model::test_utils::*,
        withdraw::api::{BulkWithdrawalRequest, WithdrawalDto, WithdrawalRequest},
    },
};

#[fixture]
fn product_365d_12apy() -> Product {
    Product {
        id: "365d_12apy".to_string(),
        cap: Cap::new(1_000_000_000_000_000_000, 500_000_000_000_000_000_000_000),
        terms: Terms::Fixed(FixedProductTerms {
            lockup_term: U64::from(31536000000),
            apy: Apy::Constant(UDecimal::new(12, 2)),
        }),
        withdrawal_fee: None,
        public_key: Some(
            "KlFClUvgnOgruryvkH8/J/Y8DERsf1VY3USK0Y93E/8="
                .as_bytes()
                .to_vec()
                .into(),
        ),
        is_enabled: true,
    }
}

#[rstest]
fn withdraw_locked_jar_before_maturity_by_not_owner(
    admin: AccountId,
    alice: AccountId,
    #[from(product_fixed)]
    #[with(365)]
    product: Product,
    #[with(vec![(0, 0)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.switch_account(context.owner.clone());
    expect_panic(&context, "Account owner is not found", || {
        context.contract().withdraw(product.id.clone());
    });

    assert_eq!(context.withdraw_all(&alice).total_amount.0, 0);
}

#[rstest]
fn withdraw_locked_jar_before_maturity_by_owner(
    admin: AccountId,
    alice: AccountId,
    #[from(product_fixed)]
    #[with(200)]
    product: Product,
    #[with(vec![(100, 0)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_ms(120);

    context.switch_account(&alice);

    assert_eq!(0, context.withdraw(&alice, &product.id).withdrawn_amount.0);
    assert_eq!(0, context.withdraw_all(&alice).total_amount.0);
}

#[rstest]
fn withdraw_locked_jar_after_maturity_by_not_owner(
    admin: AccountId,
    alice: AccountId,
    #[values(365, 400, 500)] term_in_days: u64,
    #[from(product_fixed)]
    #[with(term_in_days)]
    product: Product,
    #[with(vec![(0, 0)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_ms(term_in_days * MS_IN_DAY + 1);

    expect_panic(&context, "Account owner is not found", || {
        context.contract().withdraw(product.id);
    });

    assert_eq!(context.withdraw_all(&alice).total_amount.0, 0);
}

#[rstest]
fn withdraw_locked_jar_after_maturity_by_owner(
    admin: AccountId,
    alice: AccountId,
    #[values(365, 400, 500)] term_in_days: u64,
    #[from(product_fixed)]
    #[with(term_in_days)]
    product: Product,
    #[with(vec![(0, 0)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_ms(term_in_days * MS_IN_DAY + 1);

    assert_eq!(0, context.withdraw(&alice, &product.id).withdrawn_amount.0);
}

#[rstest]
#[should_panic(expected = "Account owner is not found")]
fn withdraw_flexible_jar_by_not_owner(
    admin: AccountId,
    alice: AccountId,
    #[from(product_flexible_10_percent)] product: Product,
    #[with(vec![(0, 0)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_days(1);
    context.contract().withdraw(product.id);
}

#[rstest]
fn withdraw_flexible_jar_by_owner_full(
    admin: AccountId,
    alice: AccountId,
    #[values(1_000_000, 1_000_000.to_otto(), 3.to_otto())] principal: TokenAmount,
    #[from(product_flexible_10_percent)] product: Product,
    #[with(vec![(0, principal)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_days(1);

    let withdrawn_amount = context.withdraw(&alice, &product.id);
    assert_eq!(principal, withdrawn_amount.withdrawn_amount.0);
    assert_eq!(0, withdrawn_amount.interest.0);

    let interest = context.contract().get_total_interest(alice.clone());
    let claimed = context.contract().claim_total(None).unwrap();

    assert_ne!(0, claimed.get_total().0);
    assert_eq!(interest.amount.total, claimed.get_total());
    assert!(context.contract().get_jars_for_account(alice).0.is_empty());
}

#[rstest]
#[case(1_000_000, 200_000)]
#[case(5_000_000, 1_000_000)]
#[case(55_001, 11_000)]
fn withdraw_fixed_jar_after_maturity_with_interest(
    admin: AccountId,
    alice: AccountId,
    #[case] principal: TokenAmount,
    #[case] target_interest: TokenAmount,
    #[from(product_1_year_20_percent)] product: Product,
    #[with(vec![(0, principal)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_ms(product.terms.get_lockup_term().unwrap() + 1);

    let withdrawn = context.withdraw(&alice, &product.id);
    assert_eq!(withdrawn.withdrawn_amount.0, principal);
    assert_eq!(withdrawn.interest.0, target_interest);
    assert_eq!(withdrawn.fee.0, 0);

    assert!(context.contract().get_jars_for_account(alice).0.is_empty());
}

#[rstest]
fn withdraw_fixed_jar_before_maturity(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent_with_fixed_fee)] product: Product,
    #[with(vec![(0, 1_000_000), (MS_IN_DAY, 2_000_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar.clone())]);

    context.set_block_timestamp_in_days(100);

    let interest = context.contract().get_total_interest(alice.clone()).amount.total.0;
    assert_ne!(0, interest);

    let withdrawn = context.withdraw(&alice, &product.id);
    assert_eq!(withdrawn.withdrawn_amount.0, jar.total_principal() - 100);
    assert_eq!(withdrawn.fee.0, 100);
    assert_eq!(withdrawn.interest.0, interest);

    assert!(context.contract().get_jars_for_account(alice.clone()).0.is_empty());
    assert_eq!(0, context.contract().get_total_interest(alice).amount.total.0);
}

#[rstest]
fn withdraw_step_jar_before_maturity_is_not_allowed(
    admin: AccountId,
    alice: AccountId,
    #[from(product_7_days_20_cap_score_based)] product: Product,
    #[with(vec![(0, 1_000_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar.clone())]);
    context.contract().get_account_mut(&alice).timezone = Timezone::hour_shift(0);

    context.set_block_timestamp_in_days(3);

    let withdrawn = context.withdraw(&alice, &product.id);
    assert_eq!(withdrawn.withdrawn_amount.0, 0);
    assert_eq!(withdrawn.interest.0, 0);

    let jar_after = context.contract().get_account(&alice).get_jar(&product.id).clone();
    assert_eq!(jar.total_principal(), jar_after.total_principal());
}

#[rstest]
fn product_with_fixed_fee(
    admin: AccountId,
    alice: AccountId,
    #[values(10, 100, 500)] fee: TokenAmount,
    #[from(product_1_year_12_percent_with_fixed_fee)]
    #[with(fee)]
    product: Product,
    #[values(1_000_000, 1_000_000.to_otto(), 3.to_otto())] principal: TokenAmount,
    #[with(vec![(0, principal)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_ms(product.terms.get_lockup_term().unwrap() + 1);
    let withdraw = context.withdraw(&alice, &product.id);
    assert_eq!(withdraw.withdrawn_amount.0, principal - fee);
    assert_eq!(withdraw.fee.0, fee);
    assert_eq!(context.contract().get_fee_amount().0, fee);
}

#[rstest]
fn test_product_with_percent_fee(
    admin: AccountId,
    alice: AccountId,
    #[values(UDecimal::new(5, 4), UDecimal::new(10_000, 5), UDecimal::new(1, 1))] fee: UDecimal,
    #[from(product_1_year_12_percent_with_percent_fee)]
    #[with(fee)]
    product: Product,
    #[values(1_000_000, 1_000_000.to_otto(), 3.to_otto())] principal: TokenAmount,
    #[with(vec![(0, principal)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_ms(product.terms.get_lockup_term().unwrap() + 1);
    let withdraw = context.withdraw(&alice, &product.id);
    let reference_fee = fee * principal;
    assert_eq!(withdraw.withdrawn_amount.0, principal - reference_fee);
    assert_eq!(withdraw.fee.0, reference_fee);
    assert_eq!(context.contract().get_fee_amount().0, reference_fee);
}

#[rstest]
fn test_failed_withdraw_promise(
    admin: AccountId,
    alice: AccountId,
    #[from(product_fixed)]
    #[with(90)]
    product: Product,
    #[with(vec![(0, 1_000_000)])] jar: Jar,
) {
    test_env_ext::set_test_future_success(false);

    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_ms(product.terms.get_lockup_term().unwrap() + 1);
    context.switch_account(&alice);

    let total_principal_before_withdrawal = context
        .contract()
        .get_account(&alice)
        .get_jar(&product.id)
        .total_principal();
    let interest_before_withdrawal = context.contract().get_total_interest(alice.clone()).amount.total.0;
    assert_ne!(0, interest_before_withdrawal);

    let withdrawn = context.withdraw(&alice, &product.id);
    assert_eq!(withdrawn.withdrawn_amount.0, 0);
    assert_eq!(withdrawn.interest.0, 0);

    let contract = context.contract();
    let jar = contract.get_account(&alice).get_jar(&product.id);
    assert_eq!(total_principal_before_withdrawal, jar.total_principal());
    assert!(!jar.is_locked);
    assert_eq!(
        interest_before_withdrawal,
        contract.get_total_interest(alice.clone()).amount.total.0
    );
}

#[rstest]
fn test_failed_withdraw_internal(
    admin: AccountId,
    alice: AccountId,
    #[from(product_fixed)]
    #[with(30)]
    product: Product,
    #[values(1_000_000, 3_000_000.to_otto(), 3.to_otto())] principal: TokenAmount,
    #[with(vec![(0, principal)])] jar: Jar,
) {
    let context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar.clone())]);

    let request = WithdrawalRequest {
        product_id: product.id.clone(),
        withdrawal: WithdrawalDto::new(jar.total_principal(), 0),
        partition_index: 0,
        interest: None,
    };
    let withdraw = context
        .contract()
        .after_withdraw_internal(alice.clone(), request, false);

    assert_eq!(withdraw.withdrawn_amount.0, 0);
    assert_eq!(withdraw.fee.0, 0);

    let current_principal = context
        .contract()
        .get_account(&alice)
        .get_jar(&product.id)
        .total_principal();
    assert_eq!(principal, current_principal);
}

#[rstest]
fn test_failed_bulk_withdraw_internal(
    admin: AccountId,
    alice: AccountId,
    #[from(product_fixed)]
    #[with(100)]
    product: Product,
    #[values(400_000, 7_000_000.to_otto(), 15.to_otto())] principal: TokenAmount,
    #[with(vec![(0, principal)])] jar: Jar,
) {
    let context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar.clone())]);

    let request = BulkWithdrawalRequest {
        requests: vec![WithdrawalRequest {
            product_id: product.id.clone(),
            withdrawal: WithdrawalDto::new(jar.total_principal(), 0),
            partition_index: 0,
            interest: None,
        }],
    };

    let withdraw = context
        .contract()
        .after_bulk_withdraw_internal(alice.clone(), request, false);

    assert!(withdraw.withdrawals.is_empty());
    assert_eq!(withdraw.total_amount.0, 0);

    let current_principal = context
        .contract()
        .get_account(&alice)
        .get_jar(&product.id)
        .total_principal();
    assert_eq!(principal, current_principal);
}

#[rstest]
fn withdraw_from_locked_jar(
    admin: AccountId,
    alice: AccountId,
    #[from(product_fixed)]
    #[with(10)]
    product: Product,
    #[with(vec![(0, 500_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar.clone())]);

    context
        .contract()
        .get_account_mut(&alice)
        .get_jar_mut(&product.id)
        .lock();

    context.set_block_timestamp_in_ms(product.terms.get_lockup_term().unwrap() + 1);

    context.switch_account(&alice);
    expect_panic(&context, "Another operation on this Jar is in progress", || {
        context.contract().withdraw(product.id.clone());
    });

    assert!(context.withdraw_all(&alice).withdrawals.is_empty());
}

#[rstest]
fn withdraw_all(
    admin: AccountId,
    alice: AccountId,
    #[values(365, 730, 90)] test_duration_id_days: u64,
    #[from(product_fixed)]
    #[with(test_duration_id_days, "regular_product")]
    regular_product: Product,
    #[from(product_fixed)]
    #[with(test_duration_id_days * 2, "long_term_product")]
    long_term_product: Product,
    #[from(product_fixed)]
    #[with(90, "illegal_product")]
    illegal_product: Product,
    #[from(jar)]
    #[with(vec![(0, 10_000_000)])]
    regular_jar: Jar,
    #[from(jar)]
    #[with(vec![(0, 2_000_000)])]
    long_term_jar: Jar,
    #[from(jar)]
    #[with(vec![(0, 300_000)])]
    mut illegal_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[
            regular_product.clone(),
            long_term_product.clone(),
            illegal_product.clone(),
        ])
        .with_latest_account(
            &alice,
            &[
                (regular_product.id, regular_jar.clone()),
                (long_term_product.id, long_term_jar.clone()),
                (illegal_product.id, illegal_jar.lock().clone()),
            ],
        );

    context.set_block_timestamp_in_days(test_duration_id_days + 1);

    context.switch_account(alice.clone());
    context.contract().claim_total(None);

    let withdrawn = context.withdraw_all(&alice);
    assert_eq!(
        regular_jar.total_principal() + long_term_jar.total_principal(),
        withdrawn.principal.0
    );

    let jars = context.contract().get_jars_for_account(alice.clone());
    assert_eq!(jars.get_principal_per_product(), vec![illegal_jar.total_principal()]);
}

#[rstest]
#[case(100, UDecimal::new(1, 2))]
#[case(200, UDecimal::new(2, 2))]
fn withdraw_all_with_fee(
    admin: AccountId,
    alice: AccountId,
    #[case] fixed_fee: TokenAmount,
    #[from(product_1_year_12_percent_with_fixed_fee)]
    #[with(fixed_fee)]
    product_with_fixed_fee: Product,
    #[case] percent_fee: UDecimal,
    #[from(product_1_year_12_percent_with_percent_fee)]
    #[with(percent_fee)]
    product_with_percent_fee: Product,
    #[from(jar)]
    #[with(vec![(0, 100.to_otto())])]
    jar_with_fixed_fee: Jar,
    #[from(jar)]
    #[with(vec![(0, 1_000.to_otto())])]
    jar_with_percent_fee: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product_with_fixed_fee.clone(), product_with_percent_fee.clone()])
        .with_latest_account(
            &alice,
            &[
                (product_with_fixed_fee.id, jar_with_fixed_fee.clone()),
                (product_with_percent_fee.id, jar_with_percent_fee.clone()),
            ],
        );

    context.set_block_timestamp_in_days(product_with_fixed_fee.terms.get_lockup_term().unwrap() + 1);
    context.switch_account(alice.clone());
    context.contract().claim_total(None);

    let withdrawn = context.withdraw_all(&alice);
    let total_fee = withdrawn.withdrawals.iter().map(|withdrawal| withdrawal.fee.0).sum();
    let expected_fee = fixed_fee + percent_fee * jar_with_percent_fee.total_principal();
    assert_eq!(expected_fee, total_fee);
    assert_eq!(expected_fee, context.contract().fee_amount);
}

#[rstest]
fn batch_withdraw_all(
    admin: AccountId,
    alice: AccountId,
    #[from(product_fixed)]
    #[with(180)]
    product: Product,
    #[from(jar)]
    #[with(vec![(0, 7_000_000), (MS_IN_DAY, 300_000), (2 * MS_IN_DAY, 20_000)])]
    jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id, jar.clone())]);

    // One day after last deposit unlock
    context.set_block_timestamp_in_ms(
        product.terms.get_lockup_term().unwrap() + jar.deposits.last().unwrap().created_at + MS_IN_DAY,
    );

    context.switch_account(alice.clone());
    context.contract().claim_total(None);
    let withdrawn = context.withdraw_all(&alice);

    let withdrawn_amount = withdrawn.withdrawals.first().unwrap().withdrawn_amount.0;
    let total_deposits_principal = jar
        .deposits
        .into_iter()
        .map(|deposit| deposit.principal)
        .sum::<TokenAmount>();
    assert_eq!(total_deposits_principal, withdrawn_amount);

    let jars = context.contract().get_jars_for_account(alice.clone());
    assert!(jars.is_empty());
}

#[rstest]
fn batch_withdraw_all_with_failed_transfer_promise(
    admin: AccountId,
    alice: AccountId,
    #[from(product_fixed)]
    #[with(180)]
    product: Product,
    #[from(product_1_year_12_percent)] another_product: Product,
    #[from(jar)]
    #[with(vec![(0, 7_000_000), (MS_IN_DAY, 300_000), (2 * MS_IN_DAY, 20_000)])]
    jar: Jar,
) {
    test_env_ext::set_test_future_success(false);

    let mut context = Context::new(admin)
        .with_products(&[product.clone(), another_product.clone()])
        .with_latest_account(
            &alice,
            &[
                (product.id.clone(), jar.clone()),
                (another_product.id.clone(), jar.clone()),
            ],
        );

    // One day after last deposit unlock
    context.set_block_timestamp_in_ms(
        product.terms.get_lockup_term().unwrap() + jar.deposits.last().unwrap().created_at + MS_IN_DAY,
    );

    context.switch_account(alice.clone());
    context.contract().claim_total(None);
    let withdrawn = context.withdraw_all(&alice);

    assert!(withdrawn.withdrawals.is_empty());
    assert_eq!(0, withdrawn.total_amount.0);

    let jars = context.contract().get_jars_for_account(alice.clone());
    assert_eq!(jar.total_principal() * 2, jars.get_total_principal());

    let account = context.contract().get_account(&alice).clone();
    assert!(!account.get_jar(&product.id).is_locked);
    assert!(!account.get_jar(&another_product.id).is_locked);
}

#[rstest]
fn batch_withdraw_partially(
    admin: AccountId,
    alice: AccountId,
    #[from(product_fixed)]
    #[with(180, "product_1")]
    product_1: Product,
    #[from(product_fixed)]
    #[with(180, "product_2")]
    product_2: Product,
    #[from(product_fixed)]
    #[with(180, "product_3")]
    product_3: Product,
    #[from(jar)]
    #[with(vec![(0, 7_000_000), (MS_IN_DAY, 300_000), (2 * MS_IN_DAY, 20_000)])]
    jar_1: Jar,
    #[from(jar)]
    #[with(vec![(0, 1_000_000), (MS_IN_DAY, 400_000)])]
    jar_2: Jar,
    #[from(jar)]
    #[with(vec![(0, 17_000_000)])]
    jar_3: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product_1.clone(), product_2.clone(), product_3.clone()])
        .with_latest_account(
            &alice,
            &[
                (product_1.id.clone(), jar_1.clone()),
                (product_2.id.clone(), jar_2.clone()),
                (product_3.id.clone(), jar_3.clone()),
            ],
        );

    // One day after last deposit unlock
    context.set_block_timestamp_in_ms(
        product_1.terms.get_lockup_term().unwrap() + jar_1.deposits.last().unwrap().created_at + MS_IN_DAY,
    );

    context.switch_account(alice.clone());
    context.contract().claim_total(None);
    let withdrawn = context.withdraw_bulk(&alice, HashSet::from([product_1.id.clone(), product_2.id.clone()]));

    let total_target_deposits_principal = [jar_1.deposits, jar_2.deposits]
        .concat()
        .into_iter()
        .map(|deposit| deposit.principal)
        .sum::<TokenAmount>();
    assert_eq!(total_target_deposits_principal, withdrawn.principal.0);

    let jars = context.contract().get_jars_for_account(alice.clone());
    assert_eq!(1, jars.0.get(&product_3.id).unwrap().len());
    assert_eq!(
        jar_3.deposits.first().unwrap().principal,
        jars.get_total_principal_for_product(&product_3.id)
    );
}

#[rstest]
fn withdraw_all_with_not_ordered_deposits(
    admin: AccountId,
    alice: AccountId,
    product_365d_12apy: Product,
    #[with(vec![
        (1721116944633, 149000000000000000000),
        (1721306451909, 87000000000000000000),
        (1721554990492, 104290000000000000000),
        (1721397526843, 60440000000000000000),
        (1720684269369, 104000000000000000000),
        (1720528438422, 180000000000000000000),
        (1720187472109, 2672810000000000000000),
        (1720887994162, 133000000000000000000),
    ])]
    jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product_365d_12apy.clone()])
        .with_v1_account(&alice, &[(product_365d_12apy.id.clone(), jar.clone())]);

    context.set_block_timestamp_in_ms(1752160593000);

    let interest = context.contract().get_total_interest(alice.clone()).amount.total.0;

    let withdrawal = context.withdraw_all(&alice);
    assert_eq!(withdrawal.withdrawals[0].withdrawn_amount.0, jar.total_principal());
    assert_eq!(withdrawal.withdrawals[0].interest.0, interest);
    assert_eq!(withdrawal.principal.0, jar.total_principal());
    assert_eq!(withdrawal.interest.0, interest);
    assert_eq!(withdrawal.total_amount.0, jar.total_principal() + interest);
}

#[rstest]
fn failed_bulk_withdraw_restores_interest(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] fixed_product: Product,
    #[from(product_flexible_10_percent)] flexible_product: Product,
    #[from(jar)]
    #[with(vec![(0, 1_000_000)])]
    fixed_jar: Jar,
    #[from(jar)]
    #[with(vec![(0, 2_000_000)])]
    flexible_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[fixed_product.clone(), flexible_product.clone()])
        .with_latest_account(
            &alice,
            &[
                (fixed_product.id.clone(), fixed_jar),
                (flexible_product.id.clone(), flexible_jar),
            ],
        );

    context.set_block_timestamp_in_days(100);
    let interest_before = context.contract().get_total_interest(alice.clone()).amount;

    test_env_ext::set_test_future_success(false);
    let withdrawn = context.withdraw_all(&alice);
    assert_eq!(withdrawn.total_amount.0, 0);

    let contract = context.contract();
    assert_eq!(interest_before, contract.get_total_interest(alice.clone()).amount);
    for jar in contract.get_account(&alice).jars.values() {
        assert!(!jar.is_locked);
    }
}

impl Context {
    fn withdraw_all(&mut self, account_id: &AccountId) -> BulkWithdrawView {
        self.withdraw_internal(account_id, None)
    }

    fn withdraw_bulk(&mut self, account_id: &AccountId, product_ids: HashSet<ProductId>) -> BulkWithdrawView {
        self.withdraw_internal(account_id, product_ids.into())
    }

    fn withdraw_internal(
        &mut self,
        account_id: &AccountId,
        product_ids: Option<HashSet<ProductId>>,
    ) -> BulkWithdrawView {
        self.switch_account(account_id);
        let result = self.contract().withdraw_all(product_ids);

        match result {
            PromiseOrValue::Promise(_) => {
                panic!("Expected value");
            }
            PromiseOrValue::Value(value) => value,
        }
    }
}

#[rstest]
fn withdraw_emits_withdraw_and_claim_events(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] product: Product,
    #[with(vec![(0, 1_000_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    context.set_block_timestamp_in_days(100);

    let withdrawn = context.withdraw(&alice, &product.id);
    assert_ne!(0, withdrawn.interest.0);

    let events = context.get_events();
    assert_eq!(2, events.len());

    let EventKind::Withdraw(_, (product_id, fee, amount)) = &events[0] else {
        panic!("Expected Withdraw event");
    };
    assert_eq!(&product.id, product_id);
    assert_eq!(0, fee.0);
    assert_eq!(1_000_000, amount.0);

    let EventKind::Claim(_, claim) = &events[1] else {
        panic!("Expected Claim event");
    };
    assert_eq!(vec![(product.id.clone(), withdrawn.interest)], claim.items);
}

#[rstest]
fn withdraw_without_accrued_interest_emits_no_claim(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] product: Product,
    #[with(vec![(0, 1_000_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar)]);

    let withdrawn = context.withdraw(&alice, &product.id);
    assert_eq!(1_000_000, withdrawn.withdrawn_amount.0);
    assert_eq!(0, withdrawn.interest.0);

    let events = context.get_events();
    assert_eq!(1, events.len());
    assert!(matches!(events[0], EventKind::Withdraw(..)));

    assert!(context.contract().get_jars_for_account(alice).0.is_empty());
}

#[rstest]
fn withdraw_succeeds_after_failed_attempt(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] product: Product,
    #[with(vec![(0, 1_000_000), (MS_IN_DAY, 3_000_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar.clone())]);

    context.set_block_timestamp_in_days(100);
    let interest = context.contract().get_total_interest(alice.clone()).amount;

    test_env_ext::set_test_future_success(false);
    let failed = context.withdraw(&alice, &product.id);
    assert_eq!(0, failed.withdrawn_amount.0);
    assert_eq!(0, failed.interest.0);

    let restored = context.contract().get_account(&alice).get_jar(&product.id).clone();
    assert_eq!(jar.deposits, restored.deposits);
    assert!(!restored.is_locked);
    assert_eq!(interest, context.contract().get_total_interest(alice.clone()).amount);

    test_env_ext::set_test_future_success(true);
    let withdrawn = context.withdraw(&alice, &product.id);
    assert_eq!(jar.total_principal(), withdrawn.withdrawn_amount.0);
    assert_eq!(interest.total, withdrawn.interest);

    assert!(context.contract().get_jars_for_account(alice).0.is_empty());
}

#[rstest]
fn withdraw_all_fixed_jars_before_maturity(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] product: Product,
    #[from(product_2_years_10_percent)] another_product: Product,
    #[from(jar)]
    #[with(vec![(0, 1_000_000)])]
    jar: Jar,
    #[from(jar)]
    #[with(vec![(0, 2_000_000), (MS_IN_DAY, 500_000)])]
    another_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone(), another_product.clone()])
        .with_latest_account(
            &alice,
            &[
                (product.id.clone(), jar.clone()),
                (another_product.id.clone(), another_jar.clone()),
            ],
        );

    context.set_block_timestamp_in_days(100);
    let interest = context.contract().get_total_interest(alice.clone()).amount;

    let withdrawn = context.withdraw_all(&alice);

    let principal = jar.total_principal() + another_jar.total_principal();
    assert_eq!(principal, withdrawn.principal.0);
    assert_eq!(interest.total, withdrawn.interest);
    assert_eq!(principal + interest.total.0, withdrawn.total_amount.0);

    assert_eq!(2, withdrawn.withdrawals.len());
    for withdrawal in &withdrawn.withdrawals {
        assert_eq!(interest.detailed[&withdrawal.product_id], withdrawal.interest);
    }

    let events = context.get_events();
    assert_eq!(2, events.len());

    let EventKind::WithdrawAll(_, withdrawals) = &events[0] else {
        panic!("Expected WithdrawAll event");
    };
    let event_principal: TokenAmount = withdrawals.iter().map(|(_, _, amount)| amount.0).sum();
    assert_eq!(principal, event_principal);

    let EventKind::Claim(_, claim) = &events[1] else {
        panic!("Expected Claim event");
    };
    assert_eq!(2, claim.items.len());
    for (product_id, amount) in &claim.items {
        assert_eq!(&interest.detailed[product_id], amount);
    }

    assert!(context.contract().get_jars_for_account(alice).0.is_empty());
}

#[rstest]
fn withdraw_all_keeps_immature_step_jar(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_12_percent)] fixed_product: Product,
    #[from(product_7_days_20_cap_score_based)] step_product: Product,
    #[from(jar)]
    #[with(vec![(0, 1_000_000)])]
    fixed_jar: Jar,
    #[from(jar)]
    #[with(vec![(0, 2_000_000)])]
    step_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[fixed_product.clone(), step_product.clone()])
        .with_latest_account(
            &alice,
            &[
                (fixed_product.id.clone(), fixed_jar.clone()),
                (step_product.id.clone(), step_jar.clone()),
            ],
        );
    context.contract().get_account_mut(&alice).timezone = Timezone::hour_shift(0);

    context.set_block_timestamp_in_days(3);

    let withdrawn = context.withdraw_all(&alice);
    assert_eq!(fixed_jar.total_principal(), withdrawn.principal.0);
    assert_ne!(0, withdrawn.interest.0);

    let step_withdrawal = withdrawn
        .withdrawals
        .iter()
        .find(|withdrawal| withdrawal.product_id == step_product.id)
        .unwrap();
    assert_eq!(0, step_withdrawal.withdrawn_amount.0);
    assert_eq!(0, step_withdrawal.interest.0);

    let jars = context.contract().get_jars_for_account(alice);
    assert_eq!(1, jars.0.len());
    assert_eq!(
        step_jar.total_principal(),
        jars.get_total_principal_for_product(&step_product.id)
    );
}
