use near_sdk::{json_types::U128, near};

use crate::{data::product::ProductId, TokenAmount};

/// The `WithdrawView` struct represents the result of a deposit jar withdrawal operation.
#[derive(Debug, PartialEq)]
#[near(serializers=[json])]
pub struct WithdrawView {
    pub product_id: ProductId,
    /// The amount of tokens that has been transferred to the user's account as part of the withdrawal.
    pub withdrawn_amount: U128,

    /// The possible fee that a user must pay for withdrawal, if it's defined by the associated Product.
    pub fee: U128,

    /// Accrued interest claimed and transferred together with the principal.
    pub claimed_amount: U128,
}

#[derive(Debug, Default)]
#[near(serializers=[json])]
pub struct BulkWithdrawView {
    /// Total net principal withdrawn across all jars.
    pub withdrawn_amount: U128,
    /// Total interest claimed along with the principal.
    pub claimed_amount: U128,
    pub withdrawals: Vec<WithdrawView>,
}

impl WithdrawView {
    #[must_use]
    pub fn new(product_id: &ProductId, amount: TokenAmount, fee: TokenAmount) -> Self {
        let net_amount = amount - fee;

        Self {
            product_id: product_id.clone(),
            withdrawn_amount: net_amount.into(),
            fee: U128(fee),
            claimed_amount: U128(0),
        }
    }

    #[must_use]
    pub fn with_claimed_amount(mut self, claimed_amount: TokenAmount) -> Self {
        self.claimed_amount = claimed_amount.into();
        self
    }
}

#[cfg(test)]
mod test {
    use near_sdk::json_types::U128;

    use crate::data::{product::ProductId, withdraw::WithdrawView};

    #[test]
    fn withdrawal_view() {
        let fee = WithdrawView::new(&ProductId::new(), 1_000_000, 100);

        assert_eq!(
            fee,
            WithdrawView {
                product_id: ProductId::new(),
                withdrawn_amount: U128(1_000_000 - 100),
                fee: U128(100),
                claimed_amount: U128(0),
            }
        );
    }
}
