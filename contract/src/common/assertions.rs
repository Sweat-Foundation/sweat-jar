use std::fmt::Display;

use near_sdk::{env, require, AccountId};

use crate::Contract;

impl Contract {
    pub(crate) fn assert_from_ft_contract(&self) {
        require!(
            env::predecessor_account_id() == self.token_account_id,
            format!("Can receive tokens only from {}", self.token_account_id)
        );
    }

    pub(crate) fn assert_migrate_from_previous_version(&self, account_id: &AccountId) {
        require!(
            account_id.clone() == self.previous_version_account_id,
            "Can migrate data only from previous version"
        );
    }

    pub(crate) fn assert_timezone_is_set(&self, account_id: &AccountId) {
        assert!(
            self.get_account(account_id).is_timezone_set(),
            "Timezone is not set for account '{account_id}'"
        );
    }
}

#[cfg_attr(all(feature = "replay-engine", not(test)), allow(dead_code))]
pub(crate) fn assert_gas<Message: Display>(gas_needed: u64, error: impl FnOnce() -> Message) {
    let gas_left = env::prepaid_gas().as_gas() - env::used_gas().as_gas();

    if gas_left < gas_needed {
        let error = error();

        env::panic_str(&format!(
            r"Not enough gas left. Consider attaching more gas to the transaction.
               {error}
               Gas left: {gas_left} Needed: {gas_needed}. Need additional {} gas",
            gas_needed - gas_left
        ));
    }
}

#[cfg(test)]
mod test {

    use near_sdk::env;

    use crate::common::{assertions::assert_gas, testing::expect_panic};

    #[test]
    fn test_assert_gas() {
        const GAS_FOR_ASSERT_CALL: u64 = 529_536_222;

        expect_panic(
            &(),
            "Not enough gas left. Consider attaching more gas to the transaction.",
            || {
                assert_gas(u64::MAX, || "Error message");
            },
        );

        let gas_left = env::prepaid_gas().as_gas() - env::used_gas().as_gas();
        expect_panic(&(), &format!("Need additional {GAS_FOR_ASSERT_CALL} gas"), || {
            assert_gas(gas_left, || "Error message");
        });

        let gas_left = env::prepaid_gas().as_gas() - env::used_gas().as_gas();
        expect_panic(&(), "Need additional 1 gas", || {
            assert_gas(gas_left - GAS_FOR_ASSERT_CALL + 1, || "Error message");
        });

        let gas_left = env::prepaid_gas().as_gas() - env::used_gas().as_gas();
        assert_gas(gas_left - GAS_FOR_ASSERT_CALL, || "Error message");

        let gas_left = env::prepaid_gas().as_gas() - env::used_gas().as_gas();
        assert_gas(gas_left - GAS_FOR_ASSERT_CALL - 1, || "Error message");
    }
}
