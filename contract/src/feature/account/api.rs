use std::{cmp::Ordering, collections::HashMap, convert::Into};

use near_plugins::{access_control_any, AccessControllable};
use near_sdk::{
    env::{self, panic_str},
    json_types::{I64, U128},
    near, AccountId,
};
use sweat_jar_model::{
    api::AccountApi,
    convert_to_days_offset,
    data::{
        account::{common::FeaturesAccess, features::Feature, view::AccountView, Account},
        jar::{AggregatedInterestView, AggregatedTokenAmountView, JarsView},
        product::{Product, ProductId, Terms},
        score::Score,
    },
    interest::{get_interest, InterestCalculator},
    ms_in_day, start_of_the_day, DailyScore, DailyScoreView, DaysOffset, ScoreIncrementProcessor, TimeHelper,
    Timestamp, Timezone, TokenAmount, SUNSET_AT, UTC,
};

use crate::{
    common::event::{emit, ApplyBoosterData, EventKind, ScoreData},
    Contract, ContractExt, Roles,
};

impl Contract {
    fn get_total_interest_for_account(&self, account_id: &AccountId) -> AggregatedInterestView {
        let mut detailed_amounts = HashMap::<ProductId, U128>::new();
        let mut total_amount: TokenAmount = 0;

        let settled_interest = self.get_settled_interest(account_id);

        let account = self.get_account(account_id);

        for (product_id, jar) in &account.jars {
            let product = self.get_product(product_id);

            let (interest, _) = product.terms.get_interest(account, jar, env::block_timestamp_ms());
            let interest = interest + settled_interest.get(product_id).map_or(0, |(amount, _)| *amount);

            detailed_amounts.insert(product_id.clone(), interest.into());
            total_amount += interest;
        }

        AggregatedInterestView {
            amount: AggregatedTokenAmountView {
                detailed: detailed_amounts,
                total: U128(total_amount),
            },
            timestamp: env::block_timestamp_ms(),
        }
    }
}

#[near]
impl AccountApi for Contract {
    fn get_jars_for_account(&self, account_id: AccountId) -> JarsView {
        if let Some(account) = self.try_get_account(&account_id) {
            return account.into();
        }

        JarsView::default()
    }

    fn get_account(&self, account_id: AccountId) -> Option<AccountView> {
        self.try_get_account(&account_id).map(|account| account.clone().into())
    }

    fn get_total_interest(&self, account_id: AccountId) -> AggregatedInterestView {
        if self.try_get_account(&account_id).is_none() {
            return AggregatedInterestView::default();
        }

        self.get_total_interest_for_account(&account_id)
    }

    #[access_control_any(roles(Roles::Maintainer))]
    fn unlock_jars_for_account(&mut self, account_id: AccountId, product_ids: Vec<ProductId>) {
        let account = self.get_account_mut(&account_id);
        for product_id in &product_ids {
            if let Some(jar) = account.jars.get_mut(product_id) {
                jar.is_locked = false;
            }
        }
    }

    #[access_control_any(roles(Roles::Oracle))]
    fn record_score(&mut self, batch: Vec<(AccountId, Vec<(Score, UTC)>)>) {
        let mut event = vec![];

        for (account_id, increments) in batch {
            self.assert_timezone_is_set(&account_id);
            self.settle_interest(&account_id);

            let account = self.get_account_mut(&account_id);

            let segmented_increments = ScoreIncrementProcessor::new(&increments, account.timezone).process();
            let normalized_increments = convert_to_days_offset(segmented_increments.valid.clone(), account.timezone);
            account.score.update(normalized_increments);

            for increment in segmented_increments.outdated {
                emit(EventKind::OldScoreWarning(increment));
            }

            event.push(ScoreData {
                account_id,
                score: segmented_increments.valid,
            });
        }

        emit(EventKind::RecordScore(event));
    }

    #[access_control_any(roles(Roles::Oracle))]
    fn apply_booster(&mut self, account_ids: Vec<AccountId>, score: Score, timestamp: UTC) {
        let mut applied = vec![];
        let mut rejected = vec![];

        for account_id in &account_ids {
            self.assert_timezone_is_set(account_id);
            self.get_account(account_id).timezone.assert_not_future(timestamp);
            self.settle_interest(account_id);

            let account = self.get_account_mut(account_id);

            let adjusted_timestamp = account.timezone.adjust(timestamp);
            let days_offset = DaysOffset::try_from(account.timezone.today().0 - adjusted_timestamp.day().0)
                .unwrap_or_else(|_| panic_str("Failed to calculate days offset"));

            if account.score.apply_booster(days_offset, score) {
                applied.push(account_id.clone());
            } else {
                rejected.push(account_id.clone());
            }
        }

        emit(EventKind::ApplyBooster(ApplyBoosterData {
            applied,
            rejected,
            timestamp,
            score,
        }));
    }

    fn get_timezone(&self, account_id: AccountId) -> Option<I64> {
        self.accounts.get(&account_id).map(|account| I64(*account.timezone))
    }

    fn get_score(&self, account_id: AccountId) -> Option<U128> {
        let account = self.try_get_account(&account_id)?;
        if !account.timezone.is_valid() {
            return None;
        }

        Some(u128::from(account.score.get_last_finalized_record(account.timezone).value).into())
    }

    fn get_boosted_score(&self, account_id: AccountId) -> Option<DailyScoreView> {
        let account = self.try_get_account(&account_id)?;
        if !account.timezone.is_valid() {
            return None;
        }

        Some(account.score.get_last_finalized_record(account.timezone).into())
    }

    #[access_control_any(roles(Roles::Oracle))]
    fn set_timezone(&mut self, account_id: AccountId, timezone: I64) {
        let account = self.get_or_create_account_mut(&account_id);
        account.try_set_timezone(Some(Timezone::new(timezone.0)));
    }

    #[access_control_any(roles(Roles::Oracle))]
    fn set_feature_enabled(&mut self, account_id: AccountId, feature: Feature, enabled: bool) {
        if self.accounts.contains_key(&account_id) {
            self.update_account_cache(&account_id, None);
        }

        let account = self.get_or_create_account_mut(&account_id);
        account.set_feature_enabled(&feature, enabled);

        emit(EventKind::SetFeatureEnabled(account_id, feature, enabled));
    }

    #[access_control_any(roles(Roles::Oracle))]
    fn batch_set_feature_enabled(&mut self, account_ids: Vec<AccountId>, feature: Feature, enabled: bool) {
        for account_id in &account_ids {
            self.update_account_cache(account_id, None);

            let account = self.get_account_mut(account_id);
            account.set_feature_enabled(&feature, enabled);
        }

        emit(EventKind::BatchSetFeatureEnabled(account_ids, feature, enabled));
    }
}

impl Contract {
    pub fn settle_interest(&mut self, account_id: &AccountId) {
        let account = self.get_account(account_id).clone();

        if !account.is_timezone_set() {
            return;
        }

        let days_since_last_update = account.score.get_days_number_since_last_update(account.timezone);

        let settled_interest = self.get_settled_interest(account_id);
        for (product_id, (interest, remainder)) in &settled_interest {
            let account = self.get_account_mut(account_id);
            let jar = account.get_jar_mut(product_id);
            let interest = jar.cache.map_or(0, |cache| cache.interest) + interest;
            let remainder = jar.claim_remainder + remainder;

            let start_of_today = start_of_the_day(env::block_timestamp_ms());

            jar.update_cache(interest, remainder, start_of_today);
        }

        match days_since_last_update.cmp(&1) {
            Ordering::Less => {}
            Ordering::Equal => self.get_account_mut(account_id).score.shift(),
            Ordering::Greater => self.get_account_mut(account_id).score.wipe(),
        }
    }

    pub fn get_settled_interest(&self, account_id: &AccountId) -> HashMap<ProductId, (TokenAmount, u64)> {
        let account = self.get_account(account_id);
        if !account.timezone.is_valid() {
            return HashMap::new();
        }

        let days_since_last_update = account.score.get_days_number_since_last_update(account.timezone);
        let start_of_today = start_of_the_day(env::block_timestamp_ms());

        let mut scores: Vec<(Timestamp, DailyScore)> = vec![];
        let score_updated_at = account.score.updated_at();

        match days_since_last_update.cmp(&1) {
            Ordering::Less => {}
            Ordering::Equal => {
                let score = account.score.get(1);
                let day_start = start_of_the_day(score_updated_at.saturating_sub(ms_in_day()));

                scores.push((day_start, score));
            }
            Ordering::Greater => {
                for i in 0..account.score.history.len() {
                    let day_start = start_of_the_day(score_updated_at.saturating_sub((i as u64) * ms_in_day()));

                    scores.push((
                        day_start,
                        account
                            .score
                            .get(u16::try_from(i).unwrap_or_else(|_| panic_str("Value is out of types bounds"))),
                    ));
                }
            }
        }

        let mut result = HashMap::new();
        for (day_start, score) in scores {
            let products =
                self.get_products_for_account(account_id, Some(|product: &Product| product.terms.is_score_based()));

            for product in &products {
                let jar = account.get_jar(&product.id);
                let cache_updated_at_relative = jar.cache.map_or(0, |cache| adjust_relative(cache.updated_at));

                if cache_updated_at_relative >= start_of_today {
                    continue;
                }

                let include_booster = matches!(product.terms, Terms::TieredScoreBased(_));

                let apy = score.to_capped_apy(get_score_cap(account, product), include_booster);
                let day_end = (day_start + ms_in_day()).min(adjust_relative(SUNSET_AT));

                let increment: (TokenAmount, u64) = jar
                    .deposits
                    .iter()
                    .map(|deposit| {
                        let deposit_created_at_relative = adjust_relative(deposit.created_at);
                        let start_time = deposit_created_at_relative
                            .max(day_start)
                            .max(cache_updated_at_relative);
                        let term = day_end.saturating_sub(start_time);

                        get_interest(deposit.principal, apy, term)
                    })
                    .fold((0, 0), |acc, (interest, remainder)| {
                        (acc.0 + interest, acc.1 + remainder)
                    });

                let current_increment: &mut (TokenAmount, u64) = result.entry(product.id.clone()).or_default();
                current_increment.0 += increment.0;
                current_increment.1 += increment.1;
            }
        }

        result
    }
}

// APY for day N is defined by score of the day N-1.
// So, to keep score recording dates and calculation dates, we should shift calculation dates to the past.
fn adjust_relative(timestamp: Timestamp) -> Timestamp {
    timestamp.saturating_sub(ms_in_day())
}

fn get_score_cap(account: &Account, product: &Product) -> Score {
    match product.terms.clone() {
        Terms::TieredScoreBased(terms) => {
            terms.get_score_cap(account.features.is_feature_enabled(&Feature::IncreasedScoreCap))
        }
        Terms::ScoreBased(terms) => terms.score_cap,
        _ => panic_str("Only ScoreBased or TieredScoreBased Product is allowed"),
    }
}
