use near_sdk::{
    env, ext_contract,
    json_types::{Base64VecU8, U128},
    near, require, AccountId, PromiseOrValue,
    PromiseOrValue::Value,
};
use primitive_types::U256;
use sweat_jar_model::{
    api::RestakeApi,
    data::{
        deposit::{DepositTicket, Purpose},
        jar::Assertions,
        product::{ProductAssertions, ProductId, ProductModelApi},
    },
    TokenAmount,
};

use crate::{
    common::{
        env::env_ext,
        event::{emit, EventKind::Restake, RestakeData},
    },
    feature::withdraw::api::WithdrawalDto,
    Contract, ContractExt,
};

#[cfg(not(any(test, feature = "replay-engine")))]
#[mutants::skip] // Covered by integration tests
pub(crate) mod gas {
    use near_sdk::Gas;

    /// Measured with `measure_after_restake_remainder_gas` (`make measure-gas`):
    /// same gas profile as withdraw, so same 4 `TGas` as `GAS_FOR_AFTER_WITHDRAW`.
    pub(crate) const GAS_FOR_AFTER_TRANSFER_REMAINDER: Gas = Gas::from_tgas(4);
}

#[derive(Debug)]
#[near(serializers=[json])]
pub(super) struct Request {
    pub account_id: AccountId,
    pub withdrawal: Option<WithdrawalDto>,
    pub deposit: DepositDto,
    pub partitions: Vec<(ProductId, usize)>,
}

#[derive(Debug)]
#[near(serializers=[json])]
pub(super) struct DepositDto {
    pub product_id: ProductId,
    pub amount: TokenAmount,
}

#[near]
impl RestakeApi for Contract {
    fn restake(
        &mut self,
        from: ProductId,
        ticket: DepositTicket,
        signature: Option<Base64VecU8>,
        amount: Option<U128>,
    ) -> PromiseOrValue<()> {
        let builder = RestakeRequestBuilder {
            account_id: env::predecessor_account_id(),
            from,
            ticket: ticket.clone(),
            target_amount: amount.map(|amount| amount.0),
        };

        self.restake_internal(&ticket, signature.as_ref(), builder)
    }

    fn restake_all(
        &mut self,
        ticket: DepositTicket,
        signature: Option<Base64VecU8>,
        amount: Option<U128>,
    ) -> PromiseOrValue<()> {
        let builder = RestakeAllRequestBuilder {
            account_id: env::predecessor_account_id(),
            ticket: ticket.clone(),
            target_amount: amount.map(|amount| amount.0),
        };

        self.restake_internal(&ticket, signature.as_ref(), builder)
    }
}

pub(super) trait RemainderTransfer {
    fn transfer_remainder(&mut self, request: Request) -> PromiseOrValue<()>;
}

#[ext_contract(ext_self)]
pub(super) trait RemainderTransferCallback {
    fn after_transfer_remainder(&mut self, request: Request) -> PromiseOrValue<()>;
}

#[near]
impl RemainderTransferCallback for Contract {
    #[private]
    fn after_transfer_remainder(&mut self, request: Request) -> PromiseOrValue<()> {
        let account_id = request.account_id.clone();
        let is_success = env_ext::is_promise_success();
        let event = Restake(
            account_id.clone(),
            RestakeData {
                is_success,
                ..RestakeData::from(&request)
            },
        );

        for (product_id, _) in &request.partitions {
            self.get_account_mut(&account_id).get_jar_mut(product_id).unlock();
        }

        if is_success {
            self.fee_amount += request.withdrawal.map_or(0, |w| w.fee);
            self.clean_up_and_deposit(request);
        }

        emit(event);

        Value(())
    }
}

impl Contract {
    fn restake_internal(
        &mut self,
        ticket: &DepositTicket,
        signature: Option<&Base64VecU8>,
        builder: impl RequestBuilder,
    ) -> PromiseOrValue<()> {
        let request = self.prepare_request_safely(ticket, signature, builder);
        let event = Restake(request.account_id.clone(), RestakeData::from(&request));

        for (product_id, _) in &request.partitions {
            self.update_jar_cache(&request.account_id, product_id);
        }

        let product = self.get_product(&ticket.product_id);
        if product.terms.is_score_based() {
            self.get_account_mut(&request.account_id)
                .try_set_timezone(ticket.timezone);
        }

        if request.withdrawal.is_none() {
            self.clean_up_and_deposit(request);
            emit(event);

            return Value(());
        }

        for (product_id, _) in &request.partitions {
            self.get_account_mut(&request.account_id).get_jar_mut(product_id).lock();
        }

        self.transfer_remainder(request)
    }

    fn prepare_request_safely(
        &self,
        ticket: &DepositTicket,
        signature: Option<&Base64VecU8>,
        builder: impl RequestBuilder,
    ) -> Request {
        let product_id = ticket.product_id.clone();
        let product = self.get_product(&product_id);
        product.assert_enabled();

        let request = builder.build(self);

        if request.deposit.amount == 0 {
            env::panic_str("Nothing to restake");
        }
        product.assert_cap(request.deposit.amount);
        self.verify(
            Purpose::Restake,
            &request.account_id,
            request.deposit.amount,
            ticket,
            signature,
        );

        request
    }

    fn clean_up_and_deposit(&mut self, request: Request) {
        let account = self.get_account_mut(&request.account_id);

        for (product_id, partition_index) in &request.partitions {
            account.get_jar_mut(product_id).clean_up_deposits(*partition_index);
        }

        self.get_account_mut(&request.account_id)
            .deposit(&request.deposit.product_id, request.deposit.amount, None);

        self.get_account_mut(&request.account_id).nonce += 1;
    }
}

trait RequestBuilder {
    fn build(&self, contract: &Contract) -> Request;
}

struct RestakeRequestBuilder {
    account_id: AccountId,
    from: ProductId,
    ticket: DepositTicket,
    target_amount: Option<TokenAmount>,
}

impl RequestBuilder for RestakeRequestBuilder {
    fn build(&self, contract: &Contract) -> Request {
        let jar = contract.get_account(&self.account_id).get_jar(&self.from);
        jar.assert_not_locked();

        let product = contract.get_product(&self.from);
        let (mature_balance, partition_index) = jar.get_liquid_balance(&product.terms);

        let deposit = DepositDto::new(self.ticket.product_id.clone(), mature_balance, self.target_amount);

        let withdrawal_amount = mature_balance - deposit.amount;
        // TODO: add test for 0 case and replace `gt` with `>`
        let withdrawal = if withdrawal_amount.gt(&0) {
            Some(WithdrawalDto {
                amount: withdrawal_amount,
                fee: product.calculate_fee(withdrawal_amount),
            })
        } else {
            None
        };

        Request {
            account_id: self.account_id.clone(),
            withdrawal,
            deposit,
            partitions: vec![(self.from.clone(), partition_index)],
        }
    }
}

struct RestakeAllRequestBuilder {
    account_id: AccountId,
    ticket: DepositTicket,
    target_amount: Option<TokenAmount>,
}

impl RequestBuilder for RestakeAllRequestBuilder {
    fn build(&self, contract: &Contract) -> Request {
        let mut partition_indices: Vec<(ProductId, usize)> = vec![];
        let mut total_mature_balance = 0;
        let mut total_fee = 0;

        for (product_id, jar) in &contract.get_account(&self.account_id).jars {
            if jar.is_locked {
                continue;
            }

            let product = contract.get_product(product_id);
            let (balance, partition_index) = jar.get_liquid_balance(&product.terms);

            // TODO: add test for 0 case and replace `gt` with `>`
            if balance.gt(&0) {
                total_mature_balance += balance;
                total_fee += product.calculate_fee(balance);
                partition_indices.push((product_id.clone(), partition_index));
            }
        }

        let deposit = DepositDto::new(self.ticket.product_id.clone(), total_mature_balance, self.target_amount);

        let withdrawal_amount = total_mature_balance - deposit.amount;
        // TODO: add test for 0 case and replace `gt` with `>`
        let withdrawal = if withdrawal_amount.gt(&0) {
            Some(WithdrawalDto {
                amount: withdrawal_amount,
                fee: mul_div_ceil(total_fee, withdrawal_amount, total_mature_balance),
            })
        } else {
            None
        };

        Request {
            account_id: self.account_id.clone(),
            withdrawal,
            deposit,
            partitions: partition_indices,
        }
    }
}

impl From<&Request> for RestakeData {
    fn from(value: &Request) -> Self {
        let from = value
            .partitions
            .iter()
            .map(|(product_id, _)| product_id.clone())
            .collect();

        Self {
            timestamp: env::block_timestamp_ms(),
            from,
            into: value.deposit.product_id.clone(),
            restaked: value.deposit.amount.into(),
            withdrawn: value.withdrawal.map_or(0.into(), |w| (w.amount - w.fee).into()),
            is_success: true,
        }
    }
}

impl DepositDto {
    fn new(product_id: ProductId, mature_balance: TokenAmount, target_amount: Option<TokenAmount>) -> Self {
        let target_amount = target_amount.unwrap_or(mature_balance);
        require!(target_amount <= mature_balance, "Not enough funds to restake");

        Self {
            product_id,
            amount: target_amount,
        }
    }
}

/// `ceil(a * b / c)` with a `U256` intermediate, since `a * b` overflows
/// `u128` for realistic SWEAT amounts. The result fits `u128` because the
/// call site guarantees `b <= c`, bounding it by `a`.
fn mul_div_ceil(a: TokenAmount, b: TokenAmount, c: TokenAmount) -> TokenAmount {
    let numerator = U256::from(a) * U256::from(b);
    let denominator = U256::from(c);
    ((numerator + denominator - U256::one()) / denominator).as_u128()
}

#[cfg(test)]
mod tests {
    use super::mul_div_ceil;

    #[test]
    fn matches_naive_formula_for_small_values() {
        assert_eq!(mul_div_ceil(100, 30, 40), (100u128 * 30).div_ceil(40));
        assert_eq!(mul_div_ceil(7, 3, 5), (7u128 * 3).div_ceil(5));
    }

    #[test]
    fn does_not_overflow_for_realistic_large_amounts() {
        // ~10^26 yocto operands: the naive product overflows u128, but the
        // true result is bounded by total_fee. Regression test for PROD-3727 (L-2).
        let total_fee: u128 = 500_000 * 10u128.pow(21);
        let withdrawal_amount: u128 = 900_000 * 10u128.pow(21);
        let total_mature_balance: u128 = 1_000_000 * 10u128.pow(21);

        let fee = mul_div_ceil(total_fee, withdrawal_amount, total_mature_balance);

        assert!(fee > 0);
        assert!(fee <= total_fee);
        assert_eq!(fee, 450_000 * 10u128.pow(21));
    }
}
