use std::collections::HashSet;

#[cfg(not(test))]
use near_sdk::require;
use near_sdk::{env::panic_str, ext_contract, near, PromiseOrValue};
use sweat_jar_model::{
    api::WithdrawApi,
    data::{
        jar::JarCompanion,
        product::{ProductId, ProductModelApi, TermsApi},
        withdraw::{BulkWithdrawView, WithdrawView},
    },
    interest::InterestCalculator,
    Timestamp, TokenAmount,
};

#[cfg(not(test))]
use crate::common::assertions::assert_gas;
use crate::common::env::env_ext;
#[cfg(test)]
use crate::common::env::test_env_ext;

#[cfg(not(test))]
pub(crate) mod gas {
    use near_sdk::Gas;

    /// Value is measured with `measure_after_withdraw_gas`
    /// (`make measure-gas`, integration-tests/tests/measure_gas.rs)
    /// Average gas for this method call don't exceed 3.4 `TGas` without interest claim.
    /// 6 here to cover interest rollback and the extra `Claim` event.
    pub(super) const GAS_FOR_AFTER_WITHDRAW: Gas = Gas::from_tgas(6);

    pub(crate) const GAS_FOR_AFTER_FEE_WITHDRAW: Gas = Gas::from_tgas(4);

    /// Value is measured with `measure_bulk_withdraw_gas`
    /// (`make measure-gas`, integration-tests/tests/measure_gas.rs)
    /// 10 `TGas` was enough for 200 jars without interest claim.
    /// 25 here to cover interest rollback snapshots and the extra `Claim` event.
    pub(super) const GAS_FOR_BULK_AFTER_WITHDRAW: Gas = Gas::from_tgas(25);
}

#[near(serializers=[json])]
#[derive(Debug, Default)]
pub(crate) struct WithdrawalRequest {
    pub product_id: ProductId,
    pub withdrawal: WithdrawalDto,
    pub partition_index: usize,
    #[serde(default)]
    pub interest: Option<InterestClaim>,
}

/// Interest claimed along with the principal. `rollback` restores the jar if the transfer fails.
#[near(serializers=[json])]
#[derive(Debug, Clone)]
pub(crate) struct InterestClaim {
    pub amount: TokenAmount,
    pub claimed_at: Timestamp,
    pub rollback: JarCompanion,
}

impl WithdrawalRequest {
    fn interest_amount(&self) -> TokenAmount {
        self.interest.as_ref().map_or(0, |interest| interest.amount)
    }

    #[cfg(not(test))]
    #[mutants::skip] // Covered by integration tests
    fn transfer_amount(&self) -> TokenAmount {
        self.withdrawal.net_amount() + self.interest_amount()
    }

    fn to_view(&self) -> WithdrawView {
        WithdrawView::new(&self.product_id, self.withdrawal.amount, self.withdrawal.fee)
            .with_claimed_amount(self.interest_amount())
    }
}

#[near(serializers=[json])]
#[derive(Debug, Default, Copy, Clone)]
pub(crate) struct WithdrawalDto {
    pub amount: TokenAmount,
    pub fee: TokenAmount,
}

impl WithdrawalDto {
    pub fn new(amount: TokenAmount, fee: TokenAmount) -> Self {
        Self { amount, fee }
    }

    #[cfg(not(test))]
    #[mutants::skip] // Covered by integration tests
    pub fn net_amount(&self) -> TokenAmount {
        // A misconfigured/migrated product must fail loudly, not underflow.
        require!(self.fee <= self.amount, "Fee exceeds amount");
        self.amount - self.fee
    }
}

#[near(serializers=[json])]
#[derive(Debug, Default)]
pub(super) struct BulkWithdrawalRequest {
    pub requests: Vec<WithdrawalRequest>,
}

#[cfg(not(test))]
#[mutants::skip] // Covered by integration tests
impl BulkWithdrawalRequest {
    fn total_transfer_amount(&self) -> TokenAmount {
        self.requests.iter().map(WithdrawalRequest::transfer_amount).sum()
    }
}

#[cfg(not(test))]
use crate::feature::ft_interface::{gas::GAS_FOR_FT_TRANSFER, FungibleTokenInterface};
use crate::{
    common::event::{emit, ClaimData, EventKind, WithdrawData},
    env, AccountId, Contract, ContractExt,
};

#[ext_contract(ext_self)]
pub(super) trait WithdrawCallbacks {
    fn after_withdraw(&mut self, account_id: AccountId, request: WithdrawalRequest) -> WithdrawView;

    fn after_bulk_withdraw(&mut self, account_id: AccountId, request: BulkWithdrawalRequest) -> BulkWithdrawView;
}

#[near]
impl WithdrawApi for Contract {
    fn withdraw(&mut self, product_id: ProductId) -> PromiseOrValue<WithdrawView> {
        let account_id = env::predecessor_account_id();

        self.get_account_mut(&account_id).get_jar_mut(&product_id).try_lock();
        self.update_jar_cache(&account_id, &product_id);

        let request = self.prepare_withdrawal(&account_id, &product_id, env::block_timestamp_ms());

        self.transfer_withdraw(&account_id, request)
    }

    fn withdraw_all(&mut self, product_ids: Option<HashSet<ProductId>>) -> PromiseOrValue<BulkWithdrawView> {
        let account_id = env::predecessor_account_id();

        self.update_account_cache(&account_id, None);

        let mut request = BulkWithdrawalRequest::default();

        let product_ids = product_ids.unwrap_or_else(|| self.get_account(&account_id).jars.keys().cloned().collect());
        let now = env::block_timestamp_ms();

        for product_id in product_ids {
            let jar = self
                .get_account(&account_id)
                .jars
                .get(&product_id)
                .unwrap_or_else(|| panic_str(&format!("No jar found for {product_id}")));
            if jar.is_locked {
                continue;
            }

            request
                .requests
                .push(self.prepare_withdrawal(&account_id, &product_id, now));
        }

        for request in &request.requests {
            self.get_account_mut(&account_id)
                .get_jar_mut(&request.product_id)
                .lock();
        }

        if request.requests.is_empty() {
            return PromiseOrValue::Value(BulkWithdrawView::default());
        }

        self.transfer_bulk_withdraw(&account_id, request)
    }
}

impl Contract {
    /// Builds a withdrawal request for the jar. If the terms claim interest on withdrawal,
    /// the interest is claimed right away and rolled back if the transfer fails.
    fn prepare_withdrawal(
        &mut self,
        account_id: &AccountId,
        product_id: &ProductId,
        now: Timestamp,
    ) -> WithdrawalRequest {
        let product = self.get_product(product_id);
        let account = self.get_account(account_id);
        let jar = account.get_jar(product_id);

        let (amount, partition_index) = jar.get_withdrawable_balance(&product.terms);
        let fee = product.calculate_fee(amount);

        let mut request = WithdrawalRequest {
            product_id: product_id.clone(),
            withdrawal: WithdrawalDto::new(amount, fee),
            partition_index,
            interest: None,
        };

        if product.terms.claims_interest_on_withdrawal() {
            let (interest, remainder) = product.terms.get_interest(account, jar, now);

            if interest > 0 {
                request.interest = Some(InterestClaim {
                    amount: interest,
                    claimed_at: now,
                    rollback: jar.to_rollback(),
                });

                self.get_account_mut(account_id)
                    .get_jar_mut(product_id)
                    .claim(remainder, now);
            }
        }

        request
    }

    pub(super) fn after_withdraw_internal(
        &mut self,
        account_id: AccountId,
        request: WithdrawalRequest,
        is_promise_success: bool,
    ) -> WithdrawView {
        if !is_promise_success {
            self.rollback_withdrawal(&account_id, &request);

            return WithdrawView::new(&request.product_id, 0, 0);
        }

        self.get_account_mut(&account_id)
            .get_jar_mut(&request.product_id)
            .unlock();
        self.clean_up(&account_id, &request);
        self.fee_amount += request.withdrawal.fee;

        let withdrawal_result = request.to_view();

        emit(EventKind::Withdraw(
            account_id.clone(),
            (
                request.product_id.clone(),
                withdrawal_result.fee,
                withdrawal_result.withdrawn_amount,
            ),
        ));
        emit_interest_claim(account_id, std::slice::from_ref(&request));

        withdrawal_result
    }

    pub(super) fn after_bulk_withdraw_internal(
        &mut self,
        account_id: AccountId,
        request: BulkWithdrawalRequest,
        is_promise_success: bool,
    ) -> BulkWithdrawView {
        if !is_promise_success {
            self.process_bulk_withdrawal_error(&account_id, request);

            return BulkWithdrawView::default();
        }

        let result = self.process_bulk_withdrawal_success(&account_id, &request);
        emit(collect_bulk_withdrawal_event_data(account_id.clone(), &result));
        emit_interest_claim(account_id, &request.requests);

        result
    }

    fn process_bulk_withdrawal_error(&mut self, account_id: &AccountId, request: BulkWithdrawalRequest) {
        for request in &request.requests {
            self.rollback_withdrawal(account_id, request);
        }
    }

    fn rollback_withdrawal(&mut self, account_id: &AccountId, request: &WithdrawalRequest) {
        let jar = self.get_account_mut(account_id).get_jar_mut(&request.product_id);
        jar.unlock();

        if let Some(interest) = &request.interest {
            jar.apply(&interest.rollback);
        }
    }

    fn process_bulk_withdrawal_success(
        &mut self,
        account_id: &AccountId,
        request: &BulkWithdrawalRequest,
    ) -> BulkWithdrawView {
        let mut result = BulkWithdrawView::default();

        for request in &request.requests {
            self.get_account_mut(account_id)
                .get_jar_mut(&request.product_id)
                .unlock();

            let deposit_withdrawal = request.to_view();

            result.withdrawn_amount.0 += deposit_withdrawal.withdrawn_amount.0;
            result.claimed_amount.0 += deposit_withdrawal.claimed_amount.0;
            result.withdrawals.push(deposit_withdrawal);
        }

        for request in &request.requests {
            self.fee_amount += request.withdrawal.fee;
            self.clean_up(account_id, request);
        }

        result
    }
}

fn collect_bulk_withdrawal_event_data(account_id: AccountId, withdrawal_result: &BulkWithdrawView) -> EventKind {
    let event_data: Vec<WithdrawData> = withdrawal_result
        .withdrawals
        .iter()
        .map(|withdrawal| {
            (
                withdrawal.product_id.clone(),
                withdrawal.fee,
                withdrawal.withdrawn_amount,
            )
        })
        .collect();

    EventKind::WithdrawAll(account_id, event_data)
}

/// Interest withdrawn along with principal is reported as a regular claim.
fn emit_interest_claim(account_id: AccountId, requests: &[WithdrawalRequest]) {
    let mut event_data: Option<ClaimData> = None;

    for request in requests {
        if let Some(interest) = &request.interest {
            event_data
                .get_or_insert_with(|| ClaimData::new(interest.claimed_at))
                .add((request.product_id.clone(), interest.amount.into()));
        }
    }

    if let Some(event_data) = event_data {
        emit(EventKind::Claim(account_id, event_data));
    }
}

impl Contract {
    fn clean_up(&mut self, account_id: &AccountId, request: &WithdrawalRequest) {
        let jar = self.get_account_mut(account_id).get_jar_mut(&request.product_id);
        jar.clean_up_deposits(request.partition_index);

        let jar = self.get_account(account_id).get_jar(&request.product_id);
        if jar.should_close() {
            self.get_account_mut(account_id).jars.remove(&request.product_id);
        }
    }
}

#[cfg(not(test))]
#[mutants::skip] // Covered by integration tests
impl Contract {
    fn transfer_withdraw(
        &mut self,
        account_id: &AccountId,
        request: WithdrawalRequest,
    ) -> PromiseOrValue<WithdrawView> {
        self.ft_contract()
            .ft_transfer(account_id, request.transfer_amount(), "withdraw")
            .then(Self::after_withdraw_call(account_id.clone(), request))
            .into()
    }

    fn transfer_bulk_withdraw(
        &mut self,
        account_id: &AccountId,
        request: BulkWithdrawalRequest,
    ) -> PromiseOrValue<BulkWithdrawView> {
        assert_gas(
            GAS_FOR_FT_TRANSFER.as_gas() + gas::GAS_FOR_BULK_AFTER_WITHDRAW.as_gas(),
            || "Not enough gas to finish withdrawal",
        );

        self.ft_contract()
            .ft_transfer(account_id, request.total_transfer_amount(), "bulk_withdraw")
            .then(Self::after_bulk_withdraw_call(account_id.clone(), request))
            .into()
    }

    fn after_withdraw_call(account_id: AccountId, request: WithdrawalRequest) -> near_sdk::Promise {
        ext_self::ext(env::current_account_id())
            .with_static_gas(gas::GAS_FOR_AFTER_WITHDRAW)
            .after_withdraw(account_id, request)
    }

    fn after_bulk_withdraw_call(account_id: AccountId, request: BulkWithdrawalRequest) -> near_sdk::Promise {
        ext_self::ext(env::current_account_id())
            .with_static_gas(gas::GAS_FOR_BULK_AFTER_WITHDRAW)
            .after_bulk_withdraw(account_id, request)
    }
}

#[cfg(test)]
impl Contract {
    fn transfer_withdraw(
        &mut self,
        account_id: &AccountId,
        request: WithdrawalRequest,
    ) -> PromiseOrValue<WithdrawView> {
        let withdrawn =
            self.after_withdraw_internal(account_id.clone(), request, test_env_ext::get_test_future_success());

        PromiseOrValue::Value(withdrawn)
    }

    fn transfer_bulk_withdraw(
        &mut self,
        account_id: &AccountId,
        request: BulkWithdrawalRequest,
    ) -> PromiseOrValue<BulkWithdrawView> {
        let withdrawn =
            self.after_bulk_withdraw_internal(account_id.clone(), request, test_env_ext::get_test_future_success());

        PromiseOrValue::Value(withdrawn)
    }
}

#[near]
#[mutants::skip] // Covered by integration tests
impl WithdrawCallbacks for Contract {
    #[private]
    fn after_withdraw(&mut self, account_id: AccountId, request: WithdrawalRequest) -> WithdrawView {
        self.after_withdraw_internal(account_id, request, env_ext::is_promise_success())
    }

    #[private]
    fn after_bulk_withdraw(&mut self, account_id: AccountId, request: BulkWithdrawalRequest) -> BulkWithdrawView {
        self.after_bulk_withdraw_internal(account_id, request, env_ext::is_promise_success())
    }
}
