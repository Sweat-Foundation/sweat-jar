#![cfg(not(any(test, feature = "replay-engine")))]

use near_sdk::{env, env::panic_str, PromiseOrValue};

use crate::{
    common::assertions::assert_gas,
    feature::{
        ft_interface::{gas::GAS_FOR_FT_TRANSFER, FungibleTokenInterface},
        restake::api::{ext_self, gas::GAS_FOR_AFTER_TRANSFER_REMAINDER, RemainderTransfer, Request},
    },
    Contract,
};

impl RemainderTransfer for Contract {
    #[mutants::skip] // Covered by integration tests
    fn transfer_remainder(&mut self, request: Request) -> PromiseOrValue<()> {
        let amount = request
            .withdrawal
            .map_or_else(|| panic_str("Transfer amount must be provided"), |w| w.net_amount());

        assert_gas(
            GAS_FOR_FT_TRANSFER.as_gas() + GAS_FOR_AFTER_TRANSFER_REMAINDER.as_gas(),
            || "Not enough gas to finish restake remainder",
        );

        self.ft_contract()
            .ft_transfer(&request.account_id, amount, "withdraw_remainder")
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_AFTER_TRANSFER_REMAINDER)
                    .after_transfer_remainder(request),
            )
            .into()
    }
}
