use near_sdk::{env, json_types::Base64VecU8, require};
use sweat_jar_primitives::UDecimal;

use super::{Product, Terms, WithdrawalFee};
use crate::{data::jar::Deposit, TokenAmount};

pub trait TermsApi {
    /// Whether principal can be withdrawn before deposits mature. Doesn't affect restaking.
    fn allows_early_withdrawal(&self) -> bool;
    /// Whether withdrawal also claims the jar's accrued interest.
    fn claims_interest_on_withdrawal(&self) -> bool;
    fn is_liquid(&self, deposit: &Deposit) -> bool;
}

impl TermsApi for Terms {
    fn allows_early_withdrawal(&self) -> bool {
        matches!(self, Terms::Fixed(_) | Terms::Flexible(_))
    }

    fn claims_interest_on_withdrawal(&self) -> bool {
        matches!(self, Terms::Fixed(_))
    }

    fn is_liquid(&self, deposit: &Deposit) -> bool {
        let now = env::block_timestamp_ms();
        match self {
            Terms::Fixed(terms) => deposit.is_liquid(now, terms.lockup_term.0),
            Terms::Flexible(_) => true,
            Terms::ScoreBased(terms) => deposit.is_liquid(now, terms.lockup_term.0),
            Terms::TieredScoreBased(terms) => deposit.is_liquid(now, terms.lockup_term.0),
        }
    }
}

pub trait ProductModelApi {
    fn get_public_key(self) -> Option<Vec<u8>>;
    fn set_public_key(&mut self, public_key: Option<Base64VecU8>);
    fn calculate_fee(&self, principal: TokenAmount) -> TokenAmount;
}

impl ProductModelApi for Product {
    fn get_public_key(self) -> Option<Vec<u8>> {
        self.public_key.map(|key| key.0)
    }

    fn set_public_key(&mut self, public_key: Option<Base64VecU8>) {
        self.public_key = public_key;
    }

    fn calculate_fee(&self, principal: TokenAmount) -> TokenAmount {
        if let Some(fee) = self.withdrawal_fee.clone() {
            return match fee {
                WithdrawalFee::Fix(amount) => amount.0,
                WithdrawalFee::Percent(percent) => percent * principal,
            };
        }

        0
    }
}

pub trait ProductAssertions {
    fn assert_cap_order(&self);
    fn assert_cap(&self, amount: TokenAmount);
    fn assert_enabled(&self);
    fn assert_fee_amount(&self);
    fn assert_score_based_product_is_protected(&self);
    fn assert_udecimal_exponents_in_range(&self);
}

impl ProductAssertions for Product {
    fn assert_cap_order(&self) {
        require!(self.cap.min() < self.cap.max(), "Cap minimum must be less than maximum");
    }

    fn assert_cap(&self, amount: TokenAmount) {
        if self.cap.min() > amount || amount > self.cap.max() {
            env::panic_str(&format!(
                "Total amount is out of product bounds: [{}..{}]",
                self.cap.min(),
                self.cap.max()
            ));
        }
    }

    fn assert_enabled(&self) {
        require!(
            self.is_enabled,
            "It's not possible to create new jars for this product: the product is disabled."
        );
    }

    /// Check if fee in new product is not too high
    fn assert_fee_amount(&self) {
        let Some(ref fee) = self.withdrawal_fee else {
            return;
        };

        let fee_ok = match fee {
            WithdrawalFee::Fix(amount) => amount.0 < self.cap.min(),
            // `Percent` is fractional: 1.0 = 100%.
            WithdrawalFee::Percent(percent) => percent.to_f32() < 1.0,
        };

        require!(
            fee_ok,
            "Fee for this product is too high. It is possible for a user to pay more in fees than they staked."
        );
    }

    fn assert_score_based_product_is_protected(&self) {
        if self.terms.is_score_based() {
            require!(self.public_key.is_some(), "Score based must be protected.");
        }
    }

    /// A product stored with an exponent beyond `UDecimal::MAX_EXPONENT`
    /// would panic on every subsequent `apy`/fee computation, so reject it
    /// up front — checking every `UDecimal` the product carries.
    fn assert_udecimal_exponents_in_range(&self) {
        let mut decimals: Vec<&UDecimal> = Vec::new();

        match &self.terms {
            Terms::Fixed(terms) => decimals.extend(terms.apy.values()),
            Terms::Flexible(terms) => decimals.extend(terms.apy.values()),
            Terms::ScoreBased(_) | Terms::TieredScoreBased(_) => {}
        }

        if let Some(WithdrawalFee::Percent(percent)) = &self.withdrawal_fee {
            decimals.push(percent);
        }

        for decimal in decimals {
            require!(
                decimal.exponent() <= UDecimal::MAX_EXPONENT,
                "UDecimal exponent out of range"
            );
        }
    }
}
