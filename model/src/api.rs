use std::collections::HashSet;

use near_sdk::{
    json_types::{Base64VecU8, I64, U128},
    AccountId,
};

use crate::{
    data::{
        account::{features::Feature, view::AccountView},
        claim::ClaimedAmountView,
        deposit::DepositTicket,
        jar::{AggregatedInterestView, JarsView},
        product::{Product, ProductId},
        score::DailyScoreView,
        withdraw::{BulkWithdrawView, WithdrawView},
    },
    Score, UTC,
};

/// The `ClaimApi` trait defines methods for claiming interest from jars within the smart contract.
pub trait ClaimApi {
    /// Claims available interest from up to 100 jars with the most interest for the calling account.
    /// If the calling account has more than 100 jars, the user will need to call this method multiple times
    /// to claim interest from all jars.
    ///
    /// * `detailed` – An optional boolean value specifying if the method must return only total amount of claimed tokens
    ///   or detailed summary for each claimed jar. Set it `true` to get a detailed result. In case of `false`
    ///   or `None` it returns only the total claimed amount.
    ///
    /// # Returns
    ///
    /// A `PromiseOrValue<ClaimedAmountView>` representing the amount of tokens claimed
    /// and probably a map containing amount of tokens claimed from each Jar. If the total available
    /// interest across all jars is zero, the returned value will also be zero and the detailed map will be empty (if requested).
    fn claim_total(&mut self, detailed: Option<bool>) -> ::near_sdk::PromiseOrValue<ClaimedAmountView>;
}

/// The `JarApi` trait defines methods for managing deposit jars and their associated data within the smart contract.
pub trait AccountApi {
    /// Retrieves information about all deposit jars associated with a given account.
    ///
    /// # Arguments
    ///
    /// * `account_id` - The `AccountId` of the account for which jar information is being retrieved.
    ///
    /// # Returns
    ///
    /// A `Vec<JarView>` containing details about all deposit jars belonging to the specified account.
    fn get_jars_for_account(&self, account_id: AccountId) -> JarsView;

    fn get_account(&self, account_id: AccountId) -> Option<AccountView>;

    /// Retrieves the total interest amount across all deposit jars for a provided account.
    ///
    /// # Arguments
    ///
    /// * `account_id` - The `AccountId` of the account for which the total interest is being retrieved.
    ///
    /// # Returns
    ///
    /// An `U128` representing the sum of interest amounts across all deposit jars for the specified account.
    /// Returns 0 if the account has no associated jars.
    fn get_total_interest(&self, account_id: AccountId) -> AggregatedInterestView;

    /// Unlocks (clears `is_locked` on) the jars for `account_id` named in `product_ids` only —
    /// does not touch any other jar the account may have.
    fn unlock_jars_for_account(&mut self, account_id: AccountId, product_ids: Vec<ProductId>);

    /// Records the score for a batch of accounts and updates their jars score accordingly.
    ///
    /// This method processes a batch of new scores for multiple accounts, updates their
    /// respective jars score, calculates interest based on the current timestamp, and emits
    /// an event with the recorded scores.
    ///
    /// # Arguments
    ///
    /// * `batch` - A vector of tuples, where each tuple contains an `AccountId` and a vector
    ///   of tuples representing the new scores and their associated timestamps (in UTC).
    ///
    /// # Panics
    ///
    /// - This function will panic if an account does not have score jars.
    /// - This function will panic if a product associated with a jar does not exist.
    fn record_score(&mut self, batch: Vec<(AccountId, Vec<(Score, UTC)>)>);

    fn apply_booster(&mut self, account_ids: Vec<AccountId>, score: Score, timestamp: UTC);

    /// Return users timezone if user has any score based jars
    fn get_timezone(&self, account_id: AccountId) -> Option<I64>;

    /// Returns current active score if user has any score based jars
    fn get_score(&self, account_id: AccountId) -> Option<U128>;

    /// Returns the account's most recently finalized daily score, including
    /// any applied booster. Unlike `get_score` (which only reflects the
    /// recorded `value`), this also surfaces `booster` so callers/tests can
    /// observe booster application directly rather than inferring it
    /// indirectly (e.g. from the `ApplyBooster` event log).
    fn get_boosted_score(&self, account_id: AccountId) -> Option<DailyScoreView>;

    fn set_timezone(&mut self, account_id: AccountId, timezone: I64);

    fn set_feature_enabled(&mut self, account_id: AccountId, feature: Feature, value: bool);

    fn batch_set_feature_enabled(&mut self, account_ids: Vec<AccountId>, feature: Feature, value: bool);
}

pub trait RestakeApi {
    /// TODO: update doc
    /// Restakes the contents of a specified deposit jar into a new jar.
    ///
    /// # Arguments
    ///
    /// * `jar_id` - The ID of the deposit jar from which the restaking is being initiated.
    ///
    /// # Returns
    ///
    /// A `JarView` containing details about the new jar created as a result of the restaking.
    ///
    /// # Panics
    ///
    /// This function may panic under the following conditions:
    /// - If the product of the original jar does not support restaking.
    /// - If the function is called by an account other than the owner of the original jar.
    /// - If the target product is not score-based.
    ///
    /// All deposits of the original jar are restaked regardless of maturity.
    /// Accrued interest stays in the original jar and is claimable separately.
    fn restake(
        &mut self,
        from: ProductId,
        ticket: DepositTicket,
        signature: Option<Base64VecU8>,
        amount: Option<U128>,
    ) -> ::near_sdk::PromiseOrValue<()>;

    /// TODO: update doc
    /// Restakes all jars for user into a Product with corresponding `product_id`.
    /// The target product must be score-based. All deposits are taken regardless of maturity.
    /// If `amount` is some, only this amount will be restaked. The rest of the principal
    /// will be withdrawn. Accrued interest stays in the original jars and is claimable separately.
    ///
    /// TODO: make with `ft_transfer_call` to support extra deposit
    fn restake_all(
        &mut self,
        ticket: DepositTicket,
        signature: Option<Base64VecU8>,
        amount: Option<U128>,
    ) -> ::near_sdk::PromiseOrValue<()>;
}

pub trait FeeApi {
    /// Returns amount of FT collected as withdrawal fee and available for withdrawal.
    fn get_fee_amount(&self) -> U128;

    /// Transfers collected fee to a dedicated account.
    fn withdraw_fee(&mut self) -> ::near_sdk::PromiseOrValue<U128>;
}

/// The `PenaltyApi` trait provides methods for applying or canceling penalties on premium jars within the smart contract.
#[deprecated]
pub trait PenaltyApi {
    /// Sets the penalty status for a specified jar.
    ///
    /// This method allows an account with the `Oracle` role to apply or cancel a penalty for a premium jar. Premium jars are those associated
    /// with products having Downgradable APY. When a user violates the terms of a premium product and a penalty is applied, the
    /// interest for the jar is calculated using a downgraded APY rate. If the terms are no longer violated, the penalty can be canceled.
    ///
    /// # Arguments
    ///
    /// * `account_id` - The account of user which owns this jar.
    /// * `jar_id` - The ID of the jar for which the penalty status is being modified.
    /// * `value` - A boolean value indicating whether the penalty should be applied (`true`) or canceled (`false`).
    ///
    /// # Panics
    ///
    /// This method will panic if the jar's associated product has a constant APY rather than a downgradable APY.
    fn set_penalty(&mut self, account_id: AccountId, value: bool);

    /// Batched version of `set_penalty`
    ///
    /// # Arguments
    ///
    /// * `jars` - List of Account IDs and their Jar IDs to which penalty must be applied.
    /// * `value` - A boolean value indicating whether the penalty should be applied (`true`) or canceled (`false`).
    ///
    /// # Panics
    ///
    /// This method will panic if the jar's associated product has a constant APY rather than a downgradable APY.
    fn batch_set_penalty(&mut self, account_ids: Vec<AccountId>, value: bool);

    fn is_penalty_applied(&self, account_id: AccountId) -> bool;
}

/// The `ProductApi` trait defines methods for managing products within the smart contract.
pub trait ProductApi {
    /// Registers a new product in the contract. This function can only be called by an account with the `ProductManager` role.
    ///
    /// # Arguments
    ///
    /// * `command` - A `RegisterProductCommand` struct containing information about the new product.
    ///
    /// # Panics
    ///
    /// This method will panic if a product with the same id already exists.
    fn register_product(&mut self, product: Product);

    /// Sets the enabled status of a specific product.
    ///
    /// This method allows modifying the enabled status of a product, which determines whether users can create
    /// jars for the specified product. If the `is_enabled` parameter is set to `true`, users will be able to create
    /// jars for the product. If set to `false`, any attempts to create jars for the product will be rejected.
    ///
    /// # Arguments
    ///
    /// * `product_id` - The ID of the product for which the enabled status is being modified.
    /// * `is_enabled` - A boolean value indicating whether the product should be enabled (`true`) or disabled (`false`).
    ///
    /// # Panics
    ///
    /// This method will panic if the provided `is_enabled` value matches the current enabled status of the product.
    fn set_enabled(&mut self, product_id: ProductId, is_enabled: bool);

    /// Sets a new public key for the specified product.
    ///
    /// This method allows replacing the existing public key associated with a product. This might be necessary
    /// in cases where a key pair is compromised, and an oracle needs to update the public key for security reasons.
    ///
    /// # Arguments
    ///
    /// * `product_id` - The ID of the product for which the public key is being replaced.
    /// * `public_key` - The new public key represented as a base64-encoded byte array.
    fn set_public_key(&mut self, product_id: ProductId, public_key: Base64VecU8);

    /// Retrieves a list of all registered products in the contract.
    ///
    /// # Returns
    ///
    /// A `Vec<ProductView>` containing information about all registered products.
    fn get_products(&self) -> Vec<Product>;
}

/// The `WithdrawApi` trait defines methods for withdrawing tokens from specific deposit jars within the smart contract.
pub trait WithdrawApi {
    /// Allows the owner of a deposit jar to withdraw a specified amount of tokens from it.
    ///
    /// # Arguments
    ///
    /// * `jar_id` - The ID of the deposit jar from which the withdrawal is being made.
    /// * `amount` - An optional `U128` value indicating the amount of tokens to withdraw. If `None` is provided,
    ///   the entire balance of the jar will be withdrawn.
    ///
    /// # Returns
    ///
    /// A `PromiseOrValue<WithdrawView>` which represents the result of the withdrawal. If the withdrawal is successful,
    /// it returns the withdrawn amount and probably fee, if it's defined by the associated Product.
    /// If there are insufficient funds or other conditions are not met, the contract might panic or
    /// return 0 for both withdrawn amount and fee.
    ///
    /// # Panics
    ///
    /// This function may panic under the following conditions:
    /// - If the caller is not the owner of the specified jar.
    /// - If the withdrawal amount exceeds the available balance in the jar.
    ///
    /// All deposits are withdrawn regardless of maturity. Accrued interest of all but Flexible
    /// jars is claimed and transferred together with the principal.
    fn withdraw(&mut self, product_id: ProductId) -> ::near_sdk::PromiseOrValue<WithdrawView>;

    /// Withdraws all jars for user, or only specified list of jars if `jars` argument is `Some`
    fn withdraw_all(&mut self, product_ids: Option<HashSet<ProductId>>)
        -> ::near_sdk::PromiseOrValue<BulkWithdrawView>;
}

#[cfg(feature = "integration-methods")]
pub trait IntegrationTestMethods {
    fn block_timestamp_ms(&self) -> near_sdk::Timestamp;
    fn bulk_create_jars(&mut self, account_id: AccountId, product_id: ProductId, principal: u128, number_of_jars: u16);
    fn set_time_scale(&mut self, time_scale: f64);
    /// Seeds the contract with a batch of step jars for a TieredScoreBased product.
    ///
    /// Each tuple in `accounts` contains:
    /// - the on-chain account ID that should own the jar,
    /// - the principal to lock (in token smallest units),
    /// - the timezone offset (hours relative to UTC) that should be associated with that account.
    ///
    /// The helper creates (or updates) the jar holder, sets the timezone, and inserts a single deposit per account
    /// using the provided `product_id`. The `deposit_timestamp_ms` allows the test to deterministically position
    /// the deposits within the accelerated timeline.
    fn seed_accounts(
        &mut self,
        product_id: ProductId,
        accounts: Vec<(AccountId, U128, crate::Timezone)>,
        deposit_timestamp_ms: u64,
    );
}
