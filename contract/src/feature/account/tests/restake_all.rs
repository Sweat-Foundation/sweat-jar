use near_sdk::{test_utils::test_env::alice, AccountId};
use rstest::rstest;
use sweat_jar_model::{
    api::{ClaimApi, ProductApi, RestakeApi},
    data::{
        deposit::{DepositMessage, DepositTicket, Purpose},
        jar::Jar,
        product::{Product, ProductModelApi},
    },
    Timezone, MS_IN_DAY, MS_IN_YEAR,
};

use crate::{
    common::{
        event::EventKind,
        testing::{accounts::*, Context},
    },
    feature::{account::model::test_utils::jar, product::model::test_utils::*},
};

#[rstest]
fn restake_all_for_single_product(
    admin: AccountId,
    #[from(product_1_year_apy_20_percent)] product: Product,
    #[from(product_1_year_12_cap_score_based)] target_product: Product,
    #[with(vec![(0, 100_000), (MS_IN_YEAR / 4, 100_000), (MS_IN_YEAR / 2, 100_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone(), target_product.clone()])
        .with_latest_account(&alice(), &[(product.id.clone(), jar.clone())]);

    let test_time = MS_IN_YEAR * 6 / 4;
    context.set_block_timestamp_in_ms(test_time);

    context.switch_account(alice());

    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };
    context.contract().restake_all(ticket, None, None);

    let contract = context.contract();
    let account = contract.get_account(&alice());

    let jar = account.get_jar(&product.id);
    assert!(jar.deposits.is_empty());
    assert_eq!(test_time, jar.cache.unwrap().updated_at);
    assert_eq!(60_000, jar.cache.unwrap().interest);

    let target_jar = account.get_jar(&target_product.id);
    assert_eq!(1, target_jar.deposits.len());
    assert_eq!(test_time, target_jar.deposits.last().unwrap().created_at);
    assert_eq!(300_000, target_jar.deposits.last().unwrap().principal);
}

#[rstest]
fn restake_all_for_different_products(
    admin: AccountId,
    #[from(product_1_year_apy_10_percent)] product: Product,
    #[from(product_1_year_apy_20_percent)] another_product: Product,
    #[from(product_1_year_12_cap_score_based)] target_product: Product,
    #[with(vec![(0, 100_000), (MS_IN_YEAR / 2, 100_000)])]
    #[from(jar)]
    jar: Jar,
    #[with(vec![(0, 200_000), (MS_IN_YEAR / 2, 200_000)])]
    #[from(jar)]
    another_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone(), another_product.clone(), target_product.clone()])
        .with_latest_account(
            &alice(),
            &[
                (product.id.clone(), jar.clone()),
                (another_product.id.clone(), another_jar.clone()),
            ],
        );

    let test_time = MS_IN_YEAR * 2;
    context.set_block_timestamp_in_ms(test_time);

    context.switch_account(alice());
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };
    context.contract().restake_all(ticket, None, None);

    let contract = context.contract();
    let account = contract.get_account(&alice());

    let jar = account.get_jar(&product.id);
    assert!(jar.deposits.is_empty());
    assert_eq!(test_time, jar.cache.unwrap().updated_at);
    assert_eq!(20_000, jar.cache.unwrap().interest);

    let another_jar = account.get_jar(&another_product.id);
    assert!(another_jar.deposits.is_empty());
    assert_eq!(test_time, another_jar.cache.unwrap().updated_at);
    assert_eq!(80_000, another_jar.cache.unwrap().interest);

    let target_jar = account.get_jar(&target_product.id);
    assert_eq!(1, target_jar.deposits.len());
    assert_eq!(test_time, target_jar.deposits.last().unwrap().created_at);
    assert_eq!(600_000, target_jar.deposits.last().unwrap().principal);
}

#[rstest]
fn restake_all_to_new_product(
    admin: AccountId,
    #[from(product_1_year_apy_10_percent)] product: Product,
    #[from(product_1_year_12_cap_score_based)] target_product: Product,
    #[with(vec![(0, 50_000), (MS_IN_YEAR / 4, 20_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone(), target_product.clone()])
        .with_latest_account(&alice(), &[(product.id.clone(), jar)]);

    let test_time = MS_IN_YEAR * 3 / 2;
    context.set_block_timestamp_in_ms(test_time);

    context.switch_account(alice());
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };
    context.contract().restake_all(ticket, None, None);

    let contract = context.contract();
    let account = contract.get_account(&alice());

    let jar = account.get_jar(&product.id);
    assert_eq!(0, jar.deposits.len());
    assert_eq!(test_time, jar.cache.unwrap().updated_at);
    assert_eq!(7_000, jar.cache.unwrap().interest);

    let target_jar = account.get_jar(&target_product.id);
    assert_eq!(1, target_jar.deposits.len());
    assert_eq!(test_time, target_jar.deposits.last().unwrap().created_at);
    assert_eq!(70_000, target_jar.deposits.last().unwrap().principal);
    assert!(target_jar.cache.is_none());
}

#[rstest]
#[should_panic(expected = "Product not_existing_product is not found")]
fn restake_all_to_not_existing_product(
    admin: AccountId,
    #[from(product_1_year_apy_10_percent)] product: Product,
    #[with(vec![(0, 500_000), (MS_IN_YEAR / 5, 700_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone()])
        .with_latest_account(&alice(), &[(product.id.clone(), jar)]);

    let test_time = MS_IN_YEAR * 3 / 2;
    context.set_block_timestamp_in_ms(test_time);

    context.switch_account(alice());
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: "not_existing_product".into(),
        valid_until: valid_until.into(),
        timezone: None,
    };
    context.contract().restake_all(ticket, None, None);
}

#[rstest]
#[should_panic(expected = "It's not possible to create new jars for this product")]
fn restake_all_to_disabled_product(
    admin: AccountId,
    #[from(product_1_year_apy_7_percent_protected)] ProtectedProduct { product, signer }: ProtectedProduct,
    #[with(vec![(0, 150_000), (MS_IN_YEAR / 3, 770_000)])] jar: Jar,
) {
    let mut context = Context::new(admin.clone())
        .with_products(&[product.clone()])
        .with_latest_account(&alice(), &[(product.id.clone(), jar.clone())]);

    let test_time = MS_IN_YEAR * 2;
    context.set_block_timestamp_in_ms(test_time);

    context.switch_account(admin);
    context.with_deposit_yocto(1, |context| context.contract().set_enabled(product.id.clone(), false));

    context.switch_account(alice());
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: product.id.clone(),
        valid_until: valid_until.into(),
        timezone: None,
    };
    let message = DepositMessage::new(
        Purpose::Restake,
        &context.owner,
        &alice(),
        &product.id,
        jar.total_principal(),
        valid_until,
        0,
    );
    let signature = signer.sign(message.as_str());

    context.contract().restake_all(ticket, Some(signature.into()), None);
}

#[rstest]
fn restake_all_with_withdrawal(
    admin: AccountId,
    #[from(product_1_year_apy_10_percent)] product: Product,
    #[from(product_1_year_12_cap_score_based)] target_product: Product,
    #[with(vec![(0, 200_000), (MS_IN_YEAR / 4, 800_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone(), target_product.clone()])
        .with_latest_account(&alice(), &[(product.id.clone(), jar)]);

    let test_time = MS_IN_YEAR * 2;
    context.set_block_timestamp_in_ms(test_time);

    context.switch_account(alice());
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };
    context.contract().restake_all(ticket, None, Some(100_000.into()));

    let contract = context.contract();
    let account = contract.get_account(&alice());

    let jar = account.get_jar(&product.id);
    assert!(jar.deposits.is_empty());
    assert_eq!(test_time, jar.cache.unwrap().updated_at);
    assert_eq!(100_000, jar.cache.unwrap().interest);

    let target_jar = account.get_jar(&target_product.id);
    assert_eq!(1, target_jar.deposits.len());
    assert_eq!(test_time, target_jar.deposits.last().unwrap().created_at);
    assert_eq!(100_000, target_jar.deposits.last().unwrap().principal);
}

#[rstest]
fn restake_all_for_multiple_products_with_withdrawal(
    admin: AccountId,
    #[from(product_1_year_12_cap_score_based)] target_product: Product,
    alice: AccountId,
    #[from(product_1_year_apy_10_percent)] product: Product,
    #[from(product_1_year_apy_20_percent)] another_product: Product,
    #[with(vec![(0, 200_000), (MS_IN_YEAR / 4, 300_000)])]
    #[from(jar)]
    jar: Jar,
    #[with(vec![(0, 400_000), (MS_IN_YEAR / 4, 500_000)])]
    #[from(jar)]
    another_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone(), another_product.clone(), target_product.clone()])
        .with_latest_account(
            &alice,
            &[
                (product.id.clone(), jar.clone()),
                (another_product.id.clone(), another_jar.clone()),
            ],
        );

    // Wait until maturity
    let restake_time = 2 * MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    // Create restake ticket
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    let principal = &jar.total_principal() + &another_jar.total_principal();
    let withdrawal_amount = 200_000;
    context.switch_account(&alice);
    context
        .contract()
        .restake_all(ticket, None, Some((principal - withdrawal_amount).into()));

    // Check emitted event
    let events = context.get_events();
    assert_eq!(events.len(), 1);

    let EventKind::Restake(_, data) = events.last().unwrap() else {
        panic!("Expected Restake event");
    };
    assert_eq!(data.restaked.0, principal - withdrawal_amount);
    assert_eq!(data.withdrawn.0, withdrawal_amount);
    assert!(data.is_success, "Restake event should have is_success=true");
}

#[rstest]
fn restake_all_for_multiple_products_with_withdrawal_and_fee(
    admin: AccountId,
    #[from(product_1_year_12_cap_score_based)] target_product: Product,
    alice: AccountId,
    #[from(product_1_year_12_percent_with_fixed_fee)] product: Product,
    #[from(product_1_year_12_percent_with_percent_fee)] another_product: Product,
    #[with(vec![(0, 200_000), (MS_IN_YEAR / 4, 300_000)])]
    #[from(jar)]
    jar: Jar,
    #[with(vec![(0, 400_000), (MS_IN_YEAR / 4, 500_000)])]
    #[from(jar)]
    another_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[product.clone(), another_product.clone(), target_product.clone()])
        .with_latest_account(
            &alice,
            &[
                (product.id.clone(), jar.clone()),
                (another_product.id.clone(), another_jar.clone()),
            ],
        );

    // Wait until maturity
    let restake_time = 2 * MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    // Create restake ticket
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    let principal = &jar.total_principal() + &another_jar.total_principal();
    let total_fee =
        product.calculate_fee(jar.total_principal()) + another_product.calculate_fee(another_jar.total_principal());
    let withdrawal_amount = 100_000;
    let target_fee = (total_fee * withdrawal_amount).div_ceil(principal);
    context.switch_account(&alice);
    context
        .contract()
        .restake_all(ticket, None, Some((principal - withdrawal_amount).into()));

    // Check emitted event
    let events = context.get_events();
    assert_eq!(events.len(), 1);

    let EventKind::Restake(_, data) = events.last().unwrap() else {
        panic!("Expected Restake event");
    };
    assert_eq!(data.restaked.0, principal - withdrawal_amount);
    assert_eq!(data.withdrawn.0, withdrawal_amount - target_fee);
    assert_eq!(context.contract().fee_amount, target_fee);
    assert!(data.is_success, "Restake event should have is_success=true");
}

/// Tests that is_success field correctly reflects failed transfer.
/// This catches mutation: delete field is_success from struct RestakeData expression
#[rstest]
fn restake_event_is_success_reflects_transfer_failure(
    admin: AccountId,
    #[from(product_1_year_12_cap_score_based)] target_product: Product,
    alice: AccountId,
    #[from(product_1_year_apy_10_percent)] product: Product,
    #[with(vec![(0, 200_000), (MS_IN_YEAR / 4, 300_000)])]
    #[from(jar)]
    jar: Jar,
) {
    use crate::common::env::test_env_ext;

    let mut context = Context::new(admin)
        .with_products(&[product.clone(), target_product.clone()])
        .with_latest_account(&alice, &[(product.id.clone(), jar.clone())]);

    // Wait until maturity
    let restake_time = 2 * MS_IN_YEAR + MS_IN_DAY;
    context.set_block_timestamp_in_ms(restake_time);

    // Create restake ticket
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    // Simulate failed transfer
    test_env_ext::set_test_future_success(false);

    context.switch_account(&alice);
    context.contract().restake_all(ticket, None, Some(100_000.into()));

    // Restore default for other tests
    test_env_ext::set_test_future_success(true);

    // Check emitted event has is_success=false
    let events = context.get_events();
    assert_eq!(events.len(), 1);

    let EventKind::Restake(_, data) = events.last().unwrap() else {
        panic!("Expected Restake event");
    };

    // This assertion catches the mutation that deletes is_success field:
    // - With field: is_success = env_ext::is_promise_success() = false
    // - Without field (mutation): is_success = RestakeData::from(&request).is_success = true
    assert!(
        !data.is_success,
        "Restake event should have is_success=false when transfer fails"
    );
}

#[rstest]
fn claim_after_restake_all_into_first_score_based_jar(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_apy_20_percent)] fixed_product: Product,
    #[from(product_1_year_12_cap_score_based)] score_based_product: Product,
    #[with(vec![(0, 100_000), (MS_IN_YEAR / 4, 100_000)])] jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[fixed_product.clone(), score_based_product.clone()])
        .with_latest_account(&alice, &[(fixed_product.id.clone(), jar.clone())]);

    let test_time = MS_IN_YEAR * 2;
    context.set_block_timestamp_in_ms(test_time);

    context.switch_account(alice.clone());
    let valid_until = MS_IN_YEAR * 10;
    let ticket = DepositTicket {
        product_id: score_based_product.id.clone(),
        valid_until: valid_until.into(),
        timezone: Some(Timezone::new(0)),
    };
    context.contract().restake_all(ticket, None, None);

    let claim_amount = context.claim_total(&alice);
    assert_eq!(40_000, claim_amount);
}

// Reproduced from https://nearblocks.io/txns/8ecF4gmLJUuq1XhUz8HvwWWz1JgmVqhvSA8gG21zvVDf
#[rstest]
fn restake_all_with_not_ordered_deposits(
    admin: AccountId,
    #[from(product_1_year_12_cap_score_based)] target_product: Product,
    alice: AccountId,
    #[from(product_fixed)]
    #[with(365, "365d_12apy")]
    product_365d_12apy: Product,
    #[from(product_fixed)]
    #[with(365, "365d_16apy_summit")]
    product_365d_16apy_summit: Product,
    #[from(product_fixed)]
    #[with(365, "365d_30apy_premium_1")]
    product_365d_30apy_premium_1: Product,
    #[from(product_fixed)]
    #[with(365, "steps_365d_20000_score_cap")]
    product_steps_365d_20000_score_cap: Product,
    #[from(product_fixed)]
    #[with(365, "the_new_year_2025")]
    product_the_new_year_2025: Product,
    #[from(jar)]
    #[with(vec![
        (1749672350517, 31740000000000000000000),
        (1732447116579, 1290000000000000000000),
        (1731857760144, 1139000000000000000000),
        (1731347020446, 1110000000000000000000),
        (1730380350311, 595000000000000000000),
        (1729078900680, 936420000000000000000),
        (1730110621788, 770000000000000000000),
        (1729378397487, 652530000000000000000),
        (1729762447943, 853000000000000000000),
        (1730848200071, 1040000000000000000000),
        (1732447498618, 29580000000000000000000),
        (1733187523708, 1377230000000000000000),
        (1747741963409, 4475000000000000000000),
        (1742390696231, 10162840000000000000000),
        (1734264148505, 12800000000000000000000),
        (1733515215507, 10540000000000000000000),
        (1734263644517, 1456230000000000000000),
        (1750687139987, 4312000000000000000000),
        (1719849346739, 1455220000000000000000),
        (1719674571411, 4037290000000000000000),
        (1719205387119, 4796250000000000000000),
        (1719503829820, 4553490000000000000000),
        (1747937851115, 1465850000000000000000),
        (1719760601010, 2205000000000000000000),
        (1750687305308, 9226000000000000000000),
        (1736860182139, 1270300000000000000000),
        (1735318565421, 13820000000000000000000),
        (1736860182139, 983777000000000000000),
        (1737737246008, 1392340000000000000000),
        (1737737246008, 1195297000000000000000),
        (1737737246008, 4188490000000000000000),
        (1737737246008, 6060671000000000000000),
        (1748429714328, 3305550000000000000000),
        (1749298467356, 1353000000000000000000),
        (1748429714328, 5188500000000000000000),
        (1742034789147, 3978750000000000000000),
        (1740330725284, 3830350000000000000000),
        (1740330725284, 5635854000000000000000),
        (1740330725284, 5470380000000000000000),
        (1740330725284, 5385160000000000000000),
        (1738394433486, 11581780000000000000000),
        (1740906631373, 3763133000000000000000),
        (1742034789147, 4063220000000000000000),
        (1742034789147, 214750000000000000000),
        (1742034789147, 550341000000000000000),
        (1742034789147, 225594000000000000000),
        (1742034789147, 2757730000000000000000),
        (1742034789147, 434140000000000000000),
        (1743506866975, 5527000000000000000000),
        (1744466757258, 6317160000000000000000),
        (1744466848231, 3703000000000000000000),
        (1745832311859, 7686260000000000000000),
        (1745832311859, 21753470000000000000000),
        (1746628518867, 1625000000000000000000),
        (1744466757258, 3848650000000000000000),
        (1745388794509, 3197720000000000000000),
        (1745474749650, 1895000000000000000000),
        (1745832623866, 925000000000000000000),
        (1747741873820, 61341390000000000000000),
        (1748429714328, 153510000000000000000),
        (1746645306597, 17369500000000000000000),
        (1747741873820, 732960000000000000000),
        (1750687398720, 1315000000000000000000),
        (1751525879030, 3953173000000000000000),
    ])]
    jar_365d_12apy: Jar,
    #[from(jar)]
    #[with(vec![
        (1728335382694, 398610000000000000000),
        (1727471930177, 624000000000000000000),
        (1728655428652, 703000000000000000000),
        (1727801219464, 700000000000000000000),
        (1728154423147, 795000000000000000000),
        (1725639839961, 2353670000000000000000),
        (1725378780452, 516850000000000000000),
        (1726490048340, 2731000000000000000000),
        (1726748254831, 372290000000000000000),
        (1727044949031, 803450000000000000000),
        (1727186235822, 300140000000000000000),
        (1727186512629, 11919470000000000000000),
        (1725980386516, 3091380000000000000000),
    ])]
    jar_365d_16apy_summit: Jar,
    #[from(jar)]
    #[with(vec![
        (1724780595569, 2116420000000000000000),
        (1724532089722, 9970000000000000000),
        (1724854882096, 515390000000000000000),
        (1725318197683, 1321260000000000000000),
        (1724530341831, 1651270000000000000000),
        (1724554324156, 116170000000000000000),
        (1725032301465, 1720640000000000000000),
        (1725126598263, 861140000000000000000),
        (1725321251852, 442500000000000000000),
        (1722530425073, 4210680000000000000000),
        (1723216608294, 4221500000000000000000),
        (1722088758428, 2038550000000000000000),
        (1724345798120, 1694330000000000000000),
        (1722796746591, 1853010000000000000000),
        (1723568942891, 3132960000000000000000),
        (1724102313135, 4700170000000000000000),
        (1724346061859, 543510000000000000000),
    ])]
    jar_365d_30apy_premium_1: Jar,
    #[from(jar)]
    #[with(vec![
        (1751552076371, 1600000000000000000000),
        (1751697008616, 170000000000000000000),
        (1752522514152, 1770000000000000000000),
    ])]
    jar_steps_365d_20000_score_cap: Jar,
    #[from(jar)]
    #[with(vec![
        (1736861012285, 48505000000000000000000),
    ])]
    jar_the_new_year_2025: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[
            product_365d_12apy.clone(),
            product_365d_16apy_summit.clone(),
            product_365d_30apy_premium_1.clone(),
            product_steps_365d_20000_score_cap.clone(),
            product_the_new_year_2025.clone(),
            target_product.clone(),
        ])
        .with_v1_account(
            &alice,
            &[
                (product_365d_12apy.id.clone(), jar_365d_12apy.clone()),
                (product_365d_16apy_summit.id.clone(), jar_365d_16apy_summit.clone()),
                (
                    product_365d_30apy_premium_1.id.clone(),
                    jar_365d_30apy_premium_1.clone(),
                ),
                (
                    product_steps_365d_20000_score_cap.id.clone(),
                    jar_steps_365d_20000_score_cap.clone(),
                ),
                (product_the_new_year_2025.id.clone(), jar_the_new_year_2025.clone()),
            ],
        );

    // Block 155253696: July 14, 2025 19:47:33 +UTC
    let target_timestamp = 1752522453000;
    context.set_block_timestamp_in_ms(target_timestamp);

    context.switch_account(&alice);
    context.contract().claim_total(None);

    let ticket = sweat_jar_model::data::deposit::DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: 0.into(),
        timezone: Some(Timezone::hour_shift(0)),
    };

    let amount_to_restake = 15_592_030_000_000_000_000_000;
    context
        .contract()
        .restake_all(ticket, None, Some(amount_to_restake.into()));

    let jars = context.contract().get_jars_for_account_detailed(&alice);
    let last_deposit = jars
        .get(&target_product.id)
        .unwrap()
        .deposits
        .iter()
        .find(|(created_at, _)| created_at.0 == target_timestamp)
        .expect("Restaked deposit not found");

    assert_eq!(last_deposit.1 .0, amount_to_restake);
}

#[rstest]
fn restake_all_takes_immature_deposits(
    admin: AccountId,
    alice: AccountId,
    #[from(product_1_year_apy_20_percent)] fixed_product: Product,
    #[from(product_7_days_20_cap_score_based)] score_based_product: Product,
    #[from(product_1_year_12_cap_score_based)] target_product: Product,
    #[with(vec![(0, 100_000), (MS_IN_DAY, 200_000)])]
    #[from(jar)]
    fixed_jar: Jar,
    #[with(vec![(98 * MS_IN_DAY, 300_000)])]
    #[from(jar)]
    score_based_jar: Jar,
) {
    let mut context = Context::new(admin)
        .with_products(&[
            fixed_product.clone(),
            score_based_product.clone(),
            target_product.clone(),
        ])
        .with_latest_account(
            &alice,
            &[
                (fixed_product.id.clone(), fixed_jar.clone()),
                (score_based_product.id.clone(), score_based_jar.clone()),
            ],
        );
    context.contract().get_account_mut(&alice).timezone = Timezone::hour_shift(0);

    let test_time = 100 * MS_IN_DAY;
    context.set_block_timestamp_in_ms(test_time);

    context.switch_account(alice.clone());
    let ticket = DepositTicket {
        product_id: target_product.id.clone(),
        valid_until: (MS_IN_YEAR * 10).into(),
        timezone: None,
    };
    context.contract().restake_all(ticket, None, None);

    let contract = context.contract();
    let account = contract.get_account(&alice);

    assert!(account.get_jar(&fixed_product.id).deposits.is_empty());
    assert_ne!(0, account.get_jar(&fixed_product.id).cache.unwrap().interest);
    assert!(account.get_jar(&score_based_product.id).deposits.is_empty());
    assert_eq!(
        fixed_jar.total_principal() + score_based_jar.total_principal(),
        account.get_jar(&target_product.id).total_principal()
    );
}
