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
    pub interest: U128,
}

#[derive(Debug, Default)]
#[near(serializers=[json])]
pub struct BulkWithdrawView {
    /// Total amount transferred to the user: `principal` + `interest`.
    pub total_amount: U128,
    /// Net principal withdrawn across all jars.
    pub principal: U128,
    /// Interest claimed along with the principal across all jars.
    pub interest: U128,
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
            interest: U128(0),
        }
    }

    #[must_use]
    pub fn with_interest(mut self, interest: TokenAmount) -> Self {
        self.interest = interest.into();
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
                interest: U128(0),
            }
        );
    }
}
