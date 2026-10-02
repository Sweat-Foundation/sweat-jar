use near_sdk::require;

use super::{Deposit, Jar, JarCache, JarCompanion};
use crate::{
    data::product::{Terms, TermsApi},
    Duration, Timestamp, TokenAmount,
};

pub trait Assertions {
    fn assert_not_locked(&self);
}

impl Assertions for Jar {
    fn assert_not_locked(&self) {
        require!(!self.is_locked, "Another operation on this Jar is in progress");
    }
}

impl Jar {
    pub fn total_principal(&self) -> TokenAmount {
        self.deposits.iter().map(|deposit| deposit.principal).sum()
    }

    /// Principal of matured deposits and the partition index splitting them off.
    pub fn get_liquid_balance(&self, terms: &Terms) -> (TokenAmount, usize) {
        let partition_index = self.deposits.partition_point(|deposit| terms.is_liquid(deposit));

        let sum = self.deposits[..partition_index]
            .iter()
            .map(|deposit| deposit.principal)
            .sum();

        (sum, partition_index)
    }

    /// Like `get_liquid_balance`, but takes every deposit if the terms allow early withdrawal.
    pub fn get_withdrawable_balance(&self, terms: &Terms) -> (TokenAmount, usize) {
        if terms.allows_early_withdrawal() {
            (self.total_principal(), self.deposits.len())
        } else {
            self.get_liquid_balance(terms)
        }
    }

    pub fn should_close(&self) -> bool {
        self.deposits.is_empty() && self.cache.is_none_or(|cache| cache.interest == 0)
    }

    pub fn lock(&mut self) -> &mut Self {
        self.is_locked = true;

        self
    }

    pub fn try_lock(&mut self) -> &mut Self {
        self.assert_not_locked();
        self.lock()
    }

    pub fn unlock(&mut self) -> &mut Self {
        self.is_locked = false;

        self
    }

    pub fn claim(&mut self, remainder: u64, now: Timestamp) -> &mut Self {
        self.claim_remainder = remainder;
        self.cache = Some(JarCache {
            updated_at: now,
            interest: 0,
        });

        self
    }

    pub fn update_cache(&mut self, interest: TokenAmount, remainder: u64, now: Timestamp) {
        self.cache = Some(JarCache {
            updated_at: now,
            interest,
        });
        self.claim_remainder = remainder;
    }

    pub fn clean_up_deposits(&mut self, partition_index: usize) {
        if partition_index == self.deposits.len() {
            self.deposits.clear();
        } else {
            self.deposits.drain(..partition_index);
        }
    }

    pub fn apply(&mut self, companion: &JarCompanion) -> &mut Self {
        if let Some(claim_remainder) = companion.claim_remainder {
            self.claim_remainder = claim_remainder;
        }

        if let Some(cache) = companion.cache {
            self.cache = cache;
        }

        if let Some(deposits) = &companion.deposits {
            self.deposits.clone_from(deposits);
        }

        if let Some(is_locked) = companion.is_locked {
            self.is_locked = is_locked;
        }

        self
    }

    pub fn to_rollback(&self) -> JarCompanion {
        JarCompanion {
            is_locked: Some(false),
            claim_remainder: Some(self.claim_remainder),
            cache: Some(self.cache),
            ..JarCompanion::default()
        }
    }
}

impl Deposit {
    pub fn new(created_at: Timestamp, principal: TokenAmount) -> Self {
        Self { created_at, principal }
    }

    pub fn is_liquid(&self, now: Timestamp, term: Duration) -> bool {
        now - self.created_at > term
    }
}

#[cfg(test)]
mod tests {
    use near_sdk::json_types::U64;
    use sweat_jar_primitives::UDecimal;

    use super::*;
    use crate::{
        data::product::{
            Apy, FixedProductTerms, FlexibleProductTerms, ScoreBasedProductTerms, TieredScoreBasedProductTerms,
        },
        ConfigurableValue, MS_IN_YEAR,
    };

    fn fixed_terms() -> Terms {
        Terms::Fixed(FixedProductTerms {
            lockup_term: U64(MS_IN_YEAR),
            apy: Apy::Constant(UDecimal::new(12, 2)),
        })
    }

    fn jar(deposits: &[(Timestamp, TokenAmount)]) -> Jar {
        Jar {
            deposits: deposits
                .iter()
                .map(|&(created_at, principal)| Deposit::new(created_at, principal))
                .collect(),
            ..Jar::default()
        }
    }

    #[test]
    fn only_fixed_jars_are_withdrawn_early_with_interest() {
        let flexible = Terms::Flexible(FlexibleProductTerms {
            apy: Apy::Constant(UDecimal::new(12, 2)),
        });
        let score_based = Terms::ScoreBased(ScoreBasedProductTerms {
            score_cap: 20_000,
            lockup_term: U64(MS_IN_YEAR),
        });
        let tiered_score_based = Terms::TieredScoreBased(TieredScoreBasedProductTerms {
            score_cap: ConfigurableValue::Constant(20_000),
            lockup_term: U64(MS_IN_YEAR),
        });

        assert!(fixed_terms().allows_early_withdrawal());
        assert!(fixed_terms().claims_interest_on_withdrawal());

        assert!(flexible.allows_early_withdrawal());
        assert!(!flexible.claims_interest_on_withdrawal());

        for terms in [score_based, tiered_score_based] {
            assert!(!terms.allows_early_withdrawal());
            assert!(!terms.claims_interest_on_withdrawal());
        }
    }

    #[test]
    fn withdrawable_balance_of_fixed_jar_includes_immature_deposits() {
        let jar = jar(&[(0, 100), (MS_IN_YEAR, 200), (2 * MS_IN_YEAR, 300)]);

        assert_eq!((600, 3), jar.get_withdrawable_balance(&fixed_terms()));
    }

    #[test]
    fn withdrawable_balance_of_empty_fixed_jar() {
        assert_eq!((0, 0), jar(&[]).get_withdrawable_balance(&fixed_terms()));
    }
}
