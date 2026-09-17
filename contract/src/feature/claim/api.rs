use std::collections::HashMap;

use near_sdk::{env, ext_contract, json_types::U128, near, require, AccountId, PromiseOrValue};
use sweat_jar_model::{
    api::ClaimApi,
    data::{
        account::{Account, AccountCompanion},
        claim::ClaimedAmountView,
        jar::{AggregatedTokenAmountView, JarCompanion},
        product::ProductId,
    },
    interest::InterestCalculator,
    TokenAmount,
};

#[cfg(not(any(test, feature = "replay-engine")))]
use crate::{common::assertions::assert_gas, feature::ft_interface::FungibleTokenInterface};
use crate::{
    common::{
        env::env_ext,
        event::{emit, ClaimData, EventKind},
    },
    Contract, ContractExt,
};

/// Hard cap on jars per `claim_total` call: the `after_claim` gas budget
/// scales with jar count, so it must stay within measured territory.
pub(super) const MAX_JARS_PER_CLAIM: usize = 200;

#[cfg(not(any(test, feature = "replay-engine")))]
#[mutants::skip] // Covered by integration tests
mod gas {
    use near_sdk::Gas;

    /// Const of after claim call with 1 jar
    pub(super) const INITIAL_GAS_FOR_AFTER_CLAIM: Gas = Gas::from_tgas(4);

    /// Cost of adding 1 additional jar in after claim call. Measured with
    /// `measure_after_claim_gas` (`make measure-gas`, integration-tests/tests/measure_gas.rs)
    pub(super) const ADDITIONAL_AFTER_CLAIM_JAR_COST: Gas = Gas::from_ggas(80);

    /// Gas to reserve for `after_claim` with `jar_count` jars
    /// (bounded by `MAX_JARS_PER_CLAIM`).
    pub(super) fn gas_for_after_claim(jar_count: u64) -> Gas {
        INITIAL_GAS_FOR_AFTER_CLAIM.saturating_add(Gas::from_gas(ADDITIONAL_AFTER_CLAIM_JAR_COST.as_gas() * jar_count))
    }
}

#[ext_contract(ext_self)]
#[cfg_attr(all(feature = "replay-engine", not(test)), allow(dead_code))]
pub trait ClaimCallbacks {
    fn after_claim(
        &mut self,
        account_id: AccountId,
        claimed_amount: ClaimedAmountView,
        account_rollback: AccountCompanion,
        event: EventKind,
    ) -> ClaimedAmountView;
}

#[near]
impl ClaimApi for Contract {
    fn claim_total(&mut self, detailed: Option<bool>) -> PromiseOrValue<ClaimedAmountView> {
        let account_id = env::predecessor_account_id();

        self.settle_interest(&account_id);

        let account = self.get_account(&account_id);
        let mut accumulator = ClaimedAmountView::new(detailed);
        let now = env::block_timestamp_ms();

        let mut rollback_jars = HashMap::new();
        let mut interest_per_jar: HashMap<ProductId, (TokenAmount, u64)> = HashMap::new();
        let mut event_data = ClaimData::new(now);

        for (product_id, jar) in &account.jars {
            if jar.is_locked {
                continue;
            }

            let product = self.get_product(product_id);
            let (interest, remainder) = product.terms.get_interest(account, jar, now);

            if interest == 0 {
                continue;
            }

            // Only claimed jars need rollback entries; the extras would bloat
            // `after_claim`'s workload past its jar_count-scaled gas budget.
            rollback_jars.insert(product_id.clone(), jar.to_rollback());
            interest_per_jar.insert(product_id.clone(), (interest, remainder));
            accumulator.add(product_id, interest);
        }

        require!(
            interest_per_jar.len() <= MAX_JARS_PER_CLAIM,
            format!("Too many jars in a single claim, max is {MAX_JARS_PER_CLAIM}")
        );
        let jar_count = interest_per_jar.len() as u64;

        let account = self.get_account_mut(&account_id);
        for (product_id, (interest, remainder)) in interest_per_jar {
            let jar = account.get_jar_mut(&product_id);
            jar.claim(remainder, now).lock();

            event_data.add((product_id.clone(), interest.into()));
        }

        let account_rollback = claim_rollback(account, rollback_jars);

        // TODO: add test for 0 case and replace `gt` with `>`
        if accumulator.get_total().0.gt(&0) {
            self.claim_interest(
                &account_id,
                accumulator,
                account_rollback,
                EventKind::Claim(account_id.clone(), event_data),
                jar_count,
            )
        } else {
            PromiseOrValue::Value(accumulator)
        }
    }
}

/// Snapshot for `after_claim`'s failure branch. Must cover every piece of
/// account state `claim_total` mutates before dispatch — a field missing here
/// silently leaks its mutation when the transfer fails (guarded by
/// `claim_rollback_snapshots_all_mutated_state`).
pub(super) fn claim_rollback(account: &Account, rollback_jars: HashMap<ProductId, JarCompanion>) -> AccountCompanion {
    AccountCompanion {
        score: account.score.into(),
        jars: rollback_jars.into(),
        timezone: account.timezone.into(),
        ..AccountCompanion::default()
    }
}

impl Contract {
    #[cfg(any(test, feature = "replay-engine"))]
    fn claim_interest(
        &mut self,
        account_id: &AccountId,
        claimed_amount: ClaimedAmountView,
        account_rollback: AccountCompanion,
        event: EventKind,
        _jar_count: u64,
    ) -> PromiseOrValue<ClaimedAmountView> {
        use crate::common::env::env_ext;

        PromiseOrValue::Value(self.after_claim_internal(
            account_id.clone(),
            claimed_amount,
            account_rollback,
            event,
            env_ext::is_promise_success(),
        ))
    }

    #[cfg(not(any(test, feature = "replay-engine")))]
    #[mutants::skip] // Covered by integration tests
    fn claim_interest(
        &mut self,
        account_id: &AccountId,
        claimed_amount: ClaimedAmountView,
        account_rollback: AccountCompanion,
        event: EventKind,
        jar_count: u64,
    ) -> PromiseOrValue<ClaimedAmountView> {
        use crate::feature::ft_interface::gas::GAS_FOR_FT_TRANSFER;

        let after_claim_gas = gas::gas_for_after_claim(jar_count);

        assert_gas(
            GAS_FOR_FT_TRANSFER.as_gas() * 2 + after_claim_gas.as_gas(),
            || "Not enough gas for claim".to_string(),
        );

        self.ft_contract()
            .ft_transfer(account_id, claimed_amount.get_total().0, "claim")
            .then(after_claim_call(
                account_id.clone(),
                claimed_amount,
                account_rollback,
                event,
                after_claim_gas,
            ))
            .into()
    }

    fn after_claim_internal(
        &mut self,
        account_id: AccountId,
        claimed_amount: ClaimedAmountView,
        account_rollback: AccountCompanion,
        event: EventKind,
        is_promise_success: bool,
    ) -> ClaimedAmountView {
        if is_promise_success {
            let account = self.accounts.get_mut(&account_id).expect("Account is not found");
            let jars = account_rollback.jars.expect("Jars are required in rollback account");

            for (product_id, _) in jars {
                let jar = account.get_jar_mut(&product_id);
                jar.unlock();

                if jar.should_close() {
                    account.jars.remove(&product_id);
                }
            }

            emit(event);

            claimed_amount
        } else {
            let account = self.get_account_mut(&account_id);
            account.apply(&account_rollback);

            match claimed_amount {
                ClaimedAmountView::Total(_) => ClaimedAmountView::Total(U128(0)),
                ClaimedAmountView::Detailed(_) => ClaimedAmountView::Detailed(AggregatedTokenAmountView::default()),
            }
        }
    }
}

#[near]
impl ClaimCallbacks for Contract {
    #[private]
    fn after_claim(
        &mut self,
        account_id: AccountId,
        claimed_amount: ClaimedAmountView,
        account_rollback: AccountCompanion,
        event: EventKind,
    ) -> ClaimedAmountView {
        self.after_claim_internal(
            account_id,
            claimed_amount,
            account_rollback,
            event,
            env_ext::is_promise_success(),
        )
    }
}

#[cfg(not(any(test, feature = "replay-engine")))]
#[mutants::skip] // Covered by integration tests
fn after_claim_call(
    account_id: AccountId,
    claimed_amount: ClaimedAmountView,
    account_rollback: AccountCompanion,
    event: EventKind,
    after_claim_gas: near_sdk::Gas,
) -> near_sdk::Promise {
    ext_self::ext(env::current_account_id())
        .with_static_gas(after_claim_gas)
        .after_claim(account_id, claimed_amount, account_rollback, event)
}
