use near_sdk::AccountId;
use rstest::rstest;
use sweat_jar_model::{
    api::{AccountApi, ProductApi, RestakeApi},
    data::{
        deposit::{DepositMessage, DepositTicket, Purpose},
        jar::Jar,
        product::Product,
    },
    Timezone, TokenAmount, MS_IN_DAY, MS_IN_YEAR,
};

use crate::{
    common::testing::{
        accounts::{admin, alice, bob, carol},
        expect_panic, Context,
    },
    feature::{
        account::model::test_utils::jar,
        product::model::test_utils::{
            product, product_7_days_20_cap_score_based, protected_score_based_product, tiered_score_based_product,
            ProductBuilder, ProtectedProduct,
        },
    },
};

#[rstest]
fn restake_by_not_owner(
    admin: AccountId,
    bob: AccountId,
    #[from(tiered_score_based_product)] product: Product,
    #[from(jar)] alice_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice(), &[(product.id.clone(), alice_jar.clone())]);

    context.switch_account(bob);
    expect_panic(&context, "Account bob.near is not found", || {
        let valid_until = MS_IN_YEAR * 10;
        let ticket = DepositTicket {
            product_id: product.id.clone(),
            valid_until: valid_until.into(),
            timezone: Some(Timezone::hour_shift(0)),
        };
        context.contract().restake(product.id.clone(), ticket, None, None);
    });

    expect_panic(&context, "Account bob.near is not found", || {
        let valid_until = MS_IN_YEAR * 10;
        let ticket = DepositTicket {
            product_id: product.id.clone(),
            valid_until: valid_until.into(),
            timezone: Some(Timezone::hour_shift(0)),
        };
        context.contract().restake_all(ticket, None, None);
    });

    context.switch_account(carol());
    expect_panic(&context, "Account carol.near is not found", || {
        let valid_until = MS_IN_YEAR * 10;
        let ticket = DepositTicket {
            product_id: product.id.clone(),
            valid_until: valid_until.into(),
            timezone: Some(Timezone::hour_shift(0)),
        };
        context.contract().restake(product.id.clone(), ticket, None, None);
    });

    expect_panic(&context, "Account carol.near is not found", || {
        let valid_until = MS_IN_YEAR * 10;
        let ticket = DepositTicket {
            product_id: product.id.clone(),
            valid_until: valid_until.into(),
            timezone: Some(Timezone::hour_shift(0)),
        };
        context.contract().restake_all(ticket, None, None);
    });
}

#[rstest]
#[should_panic(expected = "Restake is only allowed into score-based products")]
fn restake_into_fixed_product(
    alice: AccountId,
    admin: AccountId,
    product: Product,
    #[from(jar)]
    #[with(vec![(0, 1_000_000)])]
    alice_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar)]);

    context.set_block_timestamp_in_ms(MS_IN_YEAR + MS_IN_DAY);

    context.switch_account(&alice);
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: (MS_IN_YEAR * 10).into(),
        timezone: Some(Timezone::hour_shift(0)),
    };
    context.contract().restake(product.id, ticket, None, None);
}

#[rstest]
#[should_panic(expected = "It's not possible to create new jars for this product: the product is disabled.")]
fn restake_with_disabled_product(alice: AccountId, admin: AccountId, product: Product, #[from(jar)] alice_jar: Jar) {
    let mut context = Context::new(admin.clone())
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar.clone())]);

    context.switch_account(&admin);
    context.with_deposit_yocto(1, |context| context.contract().set_enabled(product.id.clone(), false));

    context.contract().products_cache.borrow_mut().clear();

    context.set_block_timestamp_in_days(366);

    context.switch_account(&alice);
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };
    context.contract().restake(product.id, ticket, None, None);
}

#[rstest]
#[should_panic(expected = "Nothing to restake")]
fn restake_empty_jar(
    alice: AccountId,
    admin: AccountId,
    #[from(tiered_score_based_product)] product: Product,
    #[from(jar)] alice_jar: Jar,
) {
    let mut context = Context::new(admin.clone())
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar.clone())]);

    context.set_block_timestamp_in_days(366);

    context.switch_account(&alice);
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };
    context.contract().restake(product.id, ticket, None, None);
}

#[rstest]
fn restake_after_maturity(
    alice: AccountId,
    admin: AccountId,
    #[from(tiered_score_based_product)] product: Product,
    #[values(100, 100_000, 2_500_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    let mut context = Context::new(admin.clone())
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar.clone())]);

    let restake_time = MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    context.switch_account(&alice);
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };
    context.contract().restake(product.id.clone(), ticket, None, None);

    let alice_jars = context.contract().get_jars_for_account(alice);
    assert_eq!(1, alice_jars.0.get(&product.id).unwrap().len());

    let jar = alice_jars.0.get(&product.id).unwrap().first().unwrap();
    assert_eq!(principal, jar.1.into());
    assert_eq!(restake_time, jar.0 .0);
}

#[rstest]
fn restake_for_protected_product_success(
    alice: AccountId,
    admin: AccountId,
    #[from(protected_score_based_product)] ProtectedProduct { product, signer }: ProtectedProduct,
    #[values(100, 100_000, 2_500_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    let mut context = Context::new(admin.clone())
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar.clone())]);

    let restake_time = MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    context.switch_account(&alice);
    let valid_until = restake_time + MS_IN_DAY;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };
    let signature = signer.sign(
        DepositMessage::new(
            Purpose::Restake,
            &context.owner,
            &alice,
            &product.id.clone(),
            principal,
            valid_until,
            0,
        )
        .as_str(),
    );
    context
        .contract()
        .restake(product.id.clone(), ticket, Some(signature.into()), None);

    let alice_jars = context.contract().get_jars_for_account(alice);
    assert_eq!(1, alice_jars.0.len());
    assert_eq!(1, alice_jars.0.get(&product.id.clone()).unwrap().len());

    let jar = alice_jars.0.get(&product.id.clone()).unwrap().first().unwrap();
    assert_eq!(principal, jar.1.into());
    assert_eq!(restake_time, jar.0 .0);
}

#[rstest]
fn sequential_restake_for_protected_product_success(
    alice: AccountId,
    admin: AccountId,
    #[from(protected_score_based_product)] ProtectedProduct { product, signer }: ProtectedProduct,
    #[values(100, 100_000, 2_500_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    let mut context = Context::new(admin.clone())
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar.clone())]);

    let restake_time = MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    context.switch_account(&alice);
    let valid_until = restake_time + MS_IN_DAY;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    let signature = signer.sign(
        DepositMessage::new(
            Purpose::Restake,
            &context.owner,
            &alice,
            &product.id.clone(),
            principal,
            valid_until,
            0,
        )
        .as_str(),
    );
    context
        .contract()
        .restake(product.id.clone(), ticket.clone(), Some(signature.into()), None);

    let alice_jars = context.contract().get_jars_for_account(alice.clone());
    assert_eq!(1, alice_jars.0.len());
    assert_eq!(1, alice_jars.0.get(&product.id.clone()).unwrap().len());

    let jar = alice_jars.0.get(&product.id.clone()).unwrap().first().unwrap();
    assert_eq!(principal, jar.1.into());
    assert_eq!(restake_time, jar.0 .0);

    // A full year has passed since the first ticket's valid_until, so the
    // second restake needs its own ticket with a fresh valid_until.
    let restake_time = restake_time + MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    let valid_until = restake_time + MS_IN_DAY;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    let signature = signer.sign(
        DepositMessage::new(
            Purpose::Restake,
            &context.owner,
            &alice,
            &product.id.clone(),
            principal,
            valid_until,
            1,
        )
        .as_str(),
    );
    context
        .contract()
        .restake(product.id.clone(), ticket.clone(), Some(signature.into()), None);

    let alice_jars = context.contract().get_jars_for_account(alice.clone());
    assert_eq!(1, alice_jars.0.len());
    assert_eq!(1, alice_jars.0.get(&product.id.clone()).unwrap().len());

    let jar = alice_jars.0.get(&product.id.clone()).unwrap().first().unwrap();
    assert_eq!(principal, jar.1.into());
    assert_eq!(restake_time, jar.0 .0);
}

#[rstest]
#[should_panic(expected = "Not matching signature")]
fn restake_for_protected_product_invalid_signature(
    alice: AccountId,
    admin: AccountId,
    #[from(protected_score_based_product)] ProtectedProduct { product, signer }: ProtectedProduct,
    #[values(100, 100_000, 2_500_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    let mut context = Context::new(admin.clone())
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar.clone())]);

    let restake_time = MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    context.switch_account(&alice);
    let valid_until = restake_time + MS_IN_DAY;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    // invalid signature – wrong amount
    let signature = signer.sign(
        DepositMessage::new(
            Purpose::Restake,
            &context.owner,
            &alice,
            &product.id,
            principal + 100,
            valid_until,
            0,
        )
        .as_str(),
    );
    context
        .contract()
        .restake(product.id, ticket, Some(signature.into()), None);
}

#[rstest]
#[should_panic(expected = "Not matching signature")]
fn restake_with_deposit_signature(
    alice: AccountId,
    admin: AccountId,
    #[from(protected_score_based_product)] ProtectedProduct { product, signer }: ProtectedProduct,
    #[values(100, 100_000, 2_500_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    let mut context = Context::new(admin.clone())
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar.clone())]);

    let restake_time = MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    context.switch_account(&alice);
    let valid_until = restake_time + MS_IN_DAY;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    // invalid signature – wrong amount
    let signature = signer.sign(
        DepositMessage::new(
            Purpose::Deposit,
            &context.owner,
            &alice,
            &product.id,
            principal + 100,
            valid_until,
            0,
        )
        .as_str(),
    );
    context
        .contract()
        .restake(product.id, ticket, Some(signature.into()), None);
}

#[rstest]
#[should_panic(expected = "Not matching signature")]
fn restake_for_protected_product_repeated_nonce(
    alice: AccountId,
    admin: AccountId,
    #[from(protected_score_based_product)]
    #[with("product_1".to_string())]
    ProtectedProduct {
        product: product_1,
        signer: signer_1,
    }: ProtectedProduct,
    #[from(protected_score_based_product)]
    #[with("product_2".to_string())]
    ProtectedProduct {
        product: product_2,
        signer: signer_2,
    }: ProtectedProduct,
    #[values(100, 100_000, 2_500_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    let mut context = Context::new(admin.clone())
        .with_products(&[product_1.clone(), product_2.clone()])
        .with_latest_account(
            &alice,
            &[
                (product_1.id.clone(), alice_jar.clone()),
                (product_2.id.clone(), alice_jar.clone()),
            ],
        );

    let restake_time = MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    context.switch_account(&alice);
    let valid_until = restake_time + MS_IN_DAY;
    let ticket = DepositTicket {
        product_id: product_1.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    let signature = signer_1.sign(
        DepositMessage::new(
            Purpose::Restake,
            &context.owner,
            &alice,
            &product_1.id,
            principal,
            valid_until,
            0,
        )
        .as_str(),
    );
    context
        .contract()
        .restake(product_1.id, ticket.clone(), Some(signature.into()), None);

    // invalid signature – repeated nonce
    let signature = signer_2.sign(
        DepositMessage::new(
            Purpose::Restake,
            &context.owner,
            &alice,
            &product_2.id,
            principal,
            valid_until,
            1,
        )
        .as_str(),
    );
    context
        .contract()
        .restake(product_2.id, ticket, Some(signature.into()), None);
}

#[rstest]
#[should_panic(expected = "Not matching signature")]
fn restake_for_protected_product_requires_signature_for_full_balance(
    alice: AccountId,
    admin: AccountId,
    #[from(protected_score_based_product)] ProtectedProduct { product, signer }: ProtectedProduct,
    #[values(100, 100_000, 2_500_000)] principal_1: TokenAmount,
    #[values(150_000, 7_000_000)] principal_2: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal_1), (MS_IN_DAY * 2, principal_2)])]
    alice_jar: Jar,
) {
    let mut context = Context::new(admin.clone())
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar)]);

    // Only the first deposit is mature, but restake takes both.
    let restake_time = MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    context.switch_account(&alice);
    let valid_until = restake_time + MS_IN_DAY;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    let signature = signer.sign(
        DepositMessage::new(
            Purpose::Restake,
            &context.owner,
            &alice,
            &product.id,
            principal_1,
            valid_until,
            0,
        )
        .as_str(),
    );

    context
        .contract()
        .restake(product.id, ticket, Some(signature.into()), None);
}

#[rstest]
#[should_panic(expected = "Not matching signature")]
fn deposit_with_outdated_nonce_after_restake(
    alice: AccountId,
    admin: AccountId,
    #[from(protected_score_based_product)] ProtectedProduct { product, signer }: ProtectedProduct,
    #[values(100_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    let mut context = Context::new(admin.clone())
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar.clone())]);

    // Wait until maturity
    let restake_time = MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    context.switch_account(&alice);
    let valid_until = restake_time + MS_IN_DAY;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    // Create signature for restake
    let nonce = 0;
    let signature = signer.sign(
        DepositMessage::new(
            Purpose::Restake,
            &context.owner,
            &alice,
            &product.id,
            principal,
            valid_until,
            nonce,
        )
        .as_str(),
    );

    // Perform restake which should increment nonce
    context
        .contract()
        .restake(product.id.clone(), ticket.clone(), Some(signature.into()), None);

    // Try to create new jar with outdated nonce (0)
    let valid_until = restake_time + MS_IN_DAY;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };
    let signature = signer.sign(
        DepositMessage::new(
            Purpose::Deposit,
            &context.owner,
            &alice,
            &product.id,
            principal,
            valid_until,
            nonce,
        )
        .as_str(),
    );

    context
        .contract()
        .deposit(alice, ticket, principal, Some(&signature.into()));
}

#[rstest]
fn restake_with_withdrawal(
    admin: AccountId,
    alice: AccountId,
    #[from(tiered_score_based_product)] product: Product,
    #[values(1_000_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    use crate::common::event::EventKind;

    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar)]);

    // Wait until maturity
    let restake_time = MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    // Create restake ticket
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    let withdrawal_amount = 100;
    context.switch_account(&alice);
    context.contract().restake(
        product.id.clone(),
        ticket,
        None,
        Some((principal - withdrawal_amount).into()),
    );

    // Check emitted event
    let events = context.get_events();
    assert_eq!(events.len(), 1);

    let EventKind::Restake(_, data) = events.last().unwrap() else {
        panic!("Expected Restake event");
    };
    assert_eq!(data.restaked.0, principal - withdrawal_amount);
    assert_eq!(data.withdrawn.0, withdrawal_amount);
}

#[rstest]
#[should_panic(expected = "Total amount is out of product bounds")]
fn restake_exceeds_target_product_cap(
    admin: AccountId,
    alice: AccountId,
    #[from(tiered_score_based_product)] product: Product,
    #[values(1_000_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    let _ = principal;
    let target_product = product.clone().with_id("target".to_string()).with_cap(1, 100);

    let mut context = Context::new(admin)
        .with_products(&[product.clone(), target_product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), alice_jar)]);

    let restake_time = MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    context.switch_account(&alice);
    context.contract().restake(product.id.clone(), ticket, None, None);
}

#[rstest]
fn restake_into_tiered_score_based_product_sets_timezone(
    admin: AccountId,
    alice: AccountId,
    #[from(product)] source_product: Product,
    #[from(tiered_score_based_product)] target_product: Product,
    #[values(1_000_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    let _ = principal;
    let mut context = Context::new(admin)
        .with_products(&[source_product.clone(), target_product.clone()])
        .with_latest_account(&alice, &[(source_product.id.clone(), alice_jar)]);

    assert!(
        !context.contract().get_account(&alice).timezone.is_valid(),
        "alice's first interaction with any product is this restake, so her timezone must not be set yet"
    );

    let restake_time = MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    let timezone = Timezone::hour_shift(3);
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(timezone),
    };

    context.switch_account(&alice);
    context
        .contract()
        .restake(source_product.id.clone(), ticket, None, None);

    assert_eq!(timezone, context.contract().get_account(&alice).timezone);
}

#[rstest]
fn restake_immature_fixed_jar_into_score_based_product(
    admin: AccountId,
    alice: AccountId,
    #[from(product)] source_product: Product,
    #[from(tiered_score_based_product)] target_product: Product,
    #[values(1_000_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[source_product.clone(), target_product.clone()])
        .with_latest_account(&alice, &[(source_product.id.clone(), alice_jar)]);

    let restake_time = 100 * MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    let interest = context.contract().get_total_interest(alice.clone()).amount.total.0;
    assert_ne!(0, interest);

    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: (MS_IN_YEAR * 10).into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    context.switch_account(&alice);
    context
        .contract()
        .restake(source_product.id.clone(), ticket, None, None);

    let contract = context.contract();
    let account = contract.get_account(&alice);

    let target_jar = account.get_jar(&target_product.id);
    assert_eq!(principal, target_jar.total_principal());
    assert_eq!(restake_time, target_jar.deposits.first().unwrap().created_at);

    // Accrued interest stays claimable in the source jar.
    let source_jar = account.get_jar(&source_product.id);
    assert!(source_jar.deposits.is_empty());
    assert_eq!(interest, source_jar.cache.unwrap().interest);
    assert_eq!(interest, contract.get_total_interest(alice).amount.total.0);
}

#[rstest]
fn restake_immature_score_based_jar_into_score_based_product(
    admin: AccountId,
    alice: AccountId,
    #[from(product_7_days_20_cap_score_based)] source_product: Product,
    #[from(tiered_score_based_product)] target_product: Product,
    #[values(1_000_000)] principal: TokenAmount,
    #[from(jar)]
    #[with(vec![(0, principal)])]
    alice_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[source_product.clone(), target_product.clone()])
        .with_latest_account(&alice, &[(source_product.id.clone(), alice_jar)]);
    context.contract().get_account_mut(&alice).timezone = Timezone::hour_shift(0);

    context.set_block_timestamp_in_days(3);

    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: (MS_IN_YEAR * 10).into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    context.switch_account(&alice);
    context
        .contract()
        .restake(source_product.id.clone(), ticket, None, None);

    let contract = context.contract();
    let account = contract.get_account(&alice);
    assert!(account.get_jar(&source_product.id).deposits.is_empty());
    assert_eq!(principal, account.get_jar(&target_product.id).total_principal());
}
