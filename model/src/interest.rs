use std::cmp;

use sweat_jar_primitives::UDecimal;

use crate::{
    data::{
        account::{common::FeaturesAccess, features::Feature, Account},
        jar::{Deposit, Jar},
        product::{
            FixedProductTerms, FlexibleProductTerms, ScoreBasedProductTerms, Terms, TieredScoreBasedProductTerms,
        },
    },
    start_of_the_day, Duration, Timestamp, ToAPY, TokenAmount, MS_IN_YEAR, SUNSET_AT, UTC,
};

pub trait InterestCalculator {
    fn get_interest(&self, account: &Account, jar: &Jar, now: Timestamp) -> (TokenAmount, u64) {
        let now = now.min(SUNSET_AT);
        let since_date = jar.cache.map(|cache| cache.updated_at);
        let apy = self.get_apy(account);
        let cached_interest = jar.cache.map_or(0, |cache| cache.interest);

        let (interest, remainder): (TokenAmount, u64) = jar
            .deposits
            .iter()
            .map(|deposit| {
                let term = self.get_interest_calculation_term(account, now, since_date, deposit);

                if term > 0 {
                    get_interest(deposit.principal, apy, term)
                } else {
                    (0, 0)
                }
            })
            .fold((0, 0), |acc, (interest, remainder)| {
                (acc.0 + interest, acc.1 + remainder)
            });

        let total_remainder = jar.claim_remainder + remainder;
        let remainder: u64 = total_remainder % MS_IN_YEAR;
        let extra_interest = u128::from(total_remainder / MS_IN_YEAR);

        (cached_interest + interest + extra_interest, remainder)
    }

    fn get_apy(&self, account: &Account) -> UDecimal;

    fn get_interest_calculation_term(
        &self,
        account: &Account,
        now: Timestamp,
        last_cached_at: Option<Timestamp>,
        deposit: &Deposit,
    ) -> Duration;
}

impl InterestCalculator for Terms {
    fn get_apy(&self, account: &Account) -> UDecimal {
        match self {
            Terms::Fixed(terms) => terms.get_apy(account),
            Terms::Flexible(terms) => terms.get_apy(account),
            Terms::ScoreBased(terms) => terms.get_apy(account),
            Terms::TieredScoreBased(terms) => terms.get_apy(account),
        }
    }

    fn get_interest_calculation_term(
        &self,
        account: &Account,
        now: Timestamp,
        last_cached_at: Option<Timestamp>,
        deposit: &Deposit,
    ) -> Duration {
        match self {
            Terms::Fixed(terms) => terms.get_interest_calculation_term(account, now, last_cached_at, deposit),
            Terms::Flexible(terms) => terms.get_interest_calculation_term(account, now, last_cached_at, deposit),
            Terms::ScoreBased(terms) => terms.get_interest_calculation_term(account, now, last_cached_at, deposit),
            Terms::TieredScoreBased(terms) => {
                terms.get_interest_calculation_term(account, now, last_cached_at, deposit)
            }
        }
    }
}

impl InterestCalculator for FixedProductTerms {
    fn get_apy(&self, account: &Account) -> UDecimal {
        self.apy
            .get_effective(account.features().is_feature_enabled(&Feature::IncreasedApy))
    }

    fn get_interest_calculation_term(
        &self,
        _account: &Account,
        now: Timestamp,
        last_cached_at: Option<Timestamp>,
        deposit: &Deposit,
    ) -> Duration {
        let since_date = last_cached_at.map_or(deposit.created_at, |cache_date| {
            cmp::max(cache_date, deposit.created_at)
        });
        let until_date = cmp::min(now, deposit.created_at + self.lockup_term.0);

        until_date.saturating_sub(since_date)
    }
}

impl InterestCalculator for FlexibleProductTerms {
    fn get_apy(&self, account: &Account) -> UDecimal {
        self.apy
            .get_effective(account.features().is_feature_enabled(&Feature::IncreasedApy))
    }

    fn get_interest_calculation_term(
        &self,
        _account: &Account,
        now: Timestamp,
        last_cached_at: Option<Timestamp>,
        deposit: &Deposit,
    ) -> Duration {
        let since_date = last_cached_at.map_or(deposit.created_at, |cache_date| {
            cmp::max(cache_date, deposit.created_at)
        });

        now - since_date
    }
}

impl InterestCalculator for ScoreBasedProductTerms {
    fn get_apy(&self, account: &Account) -> UDecimal {
        account
            .score
            .get_capped_finalized_score(account.timezone, self.score_cap)
            .to_apy()
    }

    fn get_interest_calculation_term(
        &self,
        account: &Account,
        now: Timestamp,
        last_cached_at: Option<Timestamp>,
        deposit: &Deposit,
    ) -> Timestamp {
        let start_of_today = UTC(start_of_the_day(now));
        let start_of_today = account.timezone.adjust(start_of_today).0;

        let since_date = last_cached_at.map_or(deposit.created_at, |cache_date| {
            cmp::max(cache_date, deposit.created_at)
        });
        let since_date = start_of_today.max(since_date);

        let until_date = cmp::min(now, deposit.created_at + self.lockup_term.0);

        until_date.saturating_sub(since_date)
    }
}

impl InterestCalculator for TieredScoreBasedProductTerms {
    fn get_apy(&self, account: &Account) -> UDecimal {
        let score = account.score.get_last_finalized_record(account.timezone);
        let score_cap = self.get_score_cap(account.features.is_feature_enabled(&Feature::IncreasedScoreCap));

        score.to_capped_apy(score_cap, true)
    }

    fn get_interest_calculation_term(
        &self,
        account: &Account,
        now: Timestamp,
        last_cached_at: Option<Timestamp>,
        deposit: &Deposit,
    ) -> Timestamp {
        let start_of_today = UTC(start_of_the_day(now));
        let start_of_today = account.timezone.adjust(start_of_today).0;

        let since_date = last_cached_at.map_or(deposit.created_at, |cache_date| {
            cmp::max(cache_date, deposit.created_at)
        });
        let since_date = start_of_today.max(since_date);

        let until_date = cmp::min(now, deposit.created_at + self.lockup_term.0);

        until_date.saturating_sub(since_date)
    }
}

pub fn get_interest(principal: TokenAmount, apy: UDecimal, term: Duration) -> (TokenAmount, u64) {
    let ms_in_year: u128 = MS_IN_YEAR.into();
    let term_in_milliseconds: u128 = term.into();

    let yearly_interest = apy.saturating_mul(principal);
    let interest = term_in_milliseconds.saturating_mul(yearly_interest);

    // This will never fail because `MS_IN_YEAR` is u64
    // and remainder from u64 cannot be bigger than u64 so it is safe to unwrap here.
    let remainder: u64 = (interest % ms_in_year).try_into().unwrap();
    let interest = interest / ms_in_year;

    (interest, remainder)
}
