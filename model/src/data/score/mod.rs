use near_sdk::{
    env::{block_timestamp_ms, panic_str},
    near,
};
use sweat_jar_primitives::UDecimal;

use crate::{Day, DaysOffset, Local, TimeHelper, Timestamp, Timezone, UTC};

mod common;

pub const DAYS_STORED: usize = 2;

pub type Score = u16;

pub trait ToAPY {
    fn to_apy(self) -> UDecimal;
}

impl ToAPY for Score {
    /// 1000 scores = 1%
    fn to_apy(self) -> UDecimal {
        UDecimal::new(self.into(), 5)
    }
}

impl ToAPY for u32 {
    /// 1000 scores = 1%
    fn to_apy(self) -> UDecimal {
        UDecimal::new(self.into(), 5)
    }
}

pub type ScoreIncrement = (Score, UTC);
pub type ScoreIncrements = Vec<ScoreIncrement>;

#[near(serializers=[borsh, json])]
#[derive(Default, Copy, Clone, Debug, PartialEq)]
pub struct DailyScore {
    pub value: Score,
    pub booster: Score,
}

#[near(serializers=[json])]
#[derive(Default, Debug, PartialEq, Clone)]
pub struct DailyScoreView {
    pub value: Score,
    pub booster: Score,
}

impl From<DailyScore> for DailyScoreView {
    fn from(value: DailyScore) -> Self {
        Self {
            value: value.value,
            booster: value.booster,
        }
    }
}

impl DailyScore {
    /// The compound score corresponding to 100% APY (1000 score = 1%, see
    /// [`ToAPY`]). This is the hard ceiling on `score + booster`.
    pub const MAX: u32 = 100_000;

    pub fn new(value: Score) -> Self {
        Self { value, booster: 0 }
    }

    /// Converts this record to an APY, enforcing the compound-score invariant;
    /// every score-based APY path must go through here.
    ///
    /// `value` is capped by the product's `cap`; `booster` is added on top and
    /// is deliberately not subject to `cap`. The compound sum may reach
    /// [`Self::MAX`] (100% APY) exactly and is hard-capped there, computed in
    /// `u32` so it can't wrap at `u16::MAX`.
    pub fn to_capped_apy(&self, cap: Score, include_booster: bool) -> UDecimal {
        let capped_score = self.value.min(cap);

        if include_booster {
            let compound_score = u32::from(capped_score) + u32::from(self.booster);

            return compound_score.min(DailyScore::MAX).to_apy();
        }

        capped_score.to_apy()
    }
}

#[near(serializers=[borsh, json])]
#[derive(Default, Copy, Clone, Debug, PartialEq)]
pub struct AccountScoreLegacy {
    pub updated_at: UTC,
    pub timezone: Timezone,
    pub scores: [Score; DAYS_STORED],
    pub scores_history: [Score; DAYS_STORED],
}

#[near(serializers=[borsh, json])]
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AccountScore {
    updated_at: UTC,
    pub history: [DailyScore; DAYS_STORED],
}

#[near(serializers=[json])]
#[derive(Default, Debug, PartialEq, Clone)]
pub struct AccountScoreView {
    pub updated_at: UTC,
    pub history: Vec<DailyScoreView>,
}

impl From<AccountScore> for AccountScoreView {
    fn from(value: AccountScore) -> Self {
        Self {
            updated_at: value.updated_at,
            history: value.history.iter().copied().map(DailyScoreView::from).collect(),
        }
    }
}

impl AccountScore {
    pub fn new(updated_at: UTC, history: [DailyScore; DAYS_STORED]) -> Self {
        Self { updated_at, history }
    }

    pub fn updated_at(&self) -> Timestamp {
        self.updated_at.0
    }

    pub fn get(&self, days_ago: DaysOffset) -> DailyScore {
        self.assert_in_bounds(days_ago as usize);
        self.history[days_ago as usize]
    }

    fn get_mut(&mut self, days_ago: DaysOffset) -> &mut DailyScore {
        self.assert_in_bounds(days_ago as usize);
        &mut self.history[days_ago as usize]
    }

    #[allow(dead_code)]
    fn set(&mut self, days_ago: DaysOffset, score: DailyScore) {
        self.assert_in_bounds(days_ago as usize);
        self.history[days_ago as usize] = score;
    }

    fn add(&mut self, days_ago: DaysOffset, increment: Score) {
        let score = self.get_mut(days_ago);

        score.value = score.value.saturating_add(increment);
    }

    pub fn apply_booster(&mut self, days_ago: DaysOffset, value: Score) -> bool {
        if days_ago as usize >= DAYS_STORED {
            return false;
        }

        if self.get(days_ago).booster > 0 {
            return false;
        }

        self.get_mut(days_ago).booster = value;
        self.updated_at = block_timestamp_ms().into();

        true
    }

    /// Stamps `updated_at` on the window roll — fixed in v4.2.3 (see
    /// `fix: stamp score.updated_at when settle_interest rolls the window`).
    /// A plain `replay-engine` build intentionally omits the stamp: the entire
    /// replay window (2026-03-20..2026-08-31) ran on pre-v4.2.3 contract
    /// versions (4.1.0..4.2.2), where an early claim rolling the window
    /// without stamping `updated_at` let the following `record_score` roll it
    /// a second time, discarding a day of score accrual that was never
    /// settled — real on-chain history to reproduce for reconciliation
    /// (`replay run`/`explain`, matching `actual_total_claim`), not a bug to
    /// carry forward. `corrected-score-window` restores the stamp on top of
    /// `replay-engine` — "what should have been paid" instead of "what was
    /// actually paid", for auditing the bug's cost. Neither feature ships to
    /// the live contract (see `replay-engine`'s own doc in `contract/Cargo.toml`).
    #[cfg(any(not(feature = "replay-engine"), feature = "corrected-score-window"))]
    pub fn wipe(&mut self) {
        self.history = [DailyScore::default(); DAYS_STORED];
        self.updated_at = block_timestamp_ms().into();
    }

    #[cfg(all(feature = "replay-engine", not(feature = "corrected-score-window")))]
    pub fn wipe(&mut self) {
        self.history = [DailyScore::default(); DAYS_STORED];
    }

    #[cfg(any(not(feature = "replay-engine"), feature = "corrected-score-window"))]
    pub fn shift(&mut self) {
        self.history.copy_within(0..DAYS_STORED - 1, 1);
        self.history[0] = DailyScore::default();
        self.updated_at = block_timestamp_ms().into();
    }

    #[cfg(all(feature = "replay-engine", not(feature = "corrected-score-window")))]
    pub fn shift(&mut self) {
        self.history.copy_within(0..DAYS_STORED - 1, 1);
        self.history[0] = DailyScore::default();
    }

    #[allow(clippy::unused_self)]
    fn assert_in_bounds(&self, index: usize) {
        if index >= DAYS_STORED {
            panic_str(format!("{index} is out of range. Only store {DAYS_STORED} days.").as_str());
        }
    }

    pub fn get_capped_finalized_score(&self, timezone: Timezone, total_cap: Score) -> Score {
        self.get_last_finalized_record(timezone).value.min(total_cap)
    }

    pub fn get_last_finalized_record(&self, timezone: Timezone) -> DailyScore {
        match self.get_days_number_since_last_update(timezone) {
            // Updated today => 0 offsetted day's score is still ongoing. Return last finalized value.
            0 => self.get(1),
            // Updated earlier than today => 0 offsetted day is finalized.
            1 => self.get(0),
            _ => DailyScore::default(),
        }
    }

    pub fn update(&mut self, increments: Vec<(Score, DaysOffset)>) {
        for (increment, days_ago) in increments {
            self.add(days_ago, increment);
        }

        self.updated_at = block_timestamp_ms().into();
    }

    pub fn get_update_day(&self, timezone: Timezone) -> Day {
        timezone.adjust(self.updated_at).day()
    }

    pub fn get_days_number_since_last_update(&self, timezone: Timezone) -> DaysOffset {
        DaysOffset::try_from(timezone.today().0 - self.get_update_day(timezone).0)
            .unwrap_or_else(|_| panic_str("Failed to calculate days offset"))
    }
}

pub struct ScoreIncrementProcessor<'a> {
    scores: &'a Vec<(Score, UTC)>,
    timezone: Timezone,
}

impl<'a> ScoreIncrementProcessor<'a> {
    pub fn new(scores: &'a Vec<(Score, UTC)>, timezone: Timezone) -> Self {
        Self { scores, timezone }
    }

    pub fn process(&self) -> SegmentedScoreIncrements {
        let mut result = SegmentedScoreIncrements::default();

        for increment in self.scores {
            self.verify_timestamp(increment);

            let adjusted_increment = self.adjust_timestamp(increment);
            if self.is_valid(&adjusted_increment) {
                result.valid.push(adjusted_increment);
            } else {
                result.outdated.push(adjusted_increment);
            }
        }

        result
    }

    fn verify_timestamp(&self, increment: &(Score, UTC)) {
        self.timezone.assert_not_future(increment.1);
    }

    fn adjust_timestamp(&self, increment: &(Score, UTC)) -> (Score, Local) {
        (increment.0, self.timezone.adjust(increment.1))
    }

    fn is_valid(&self, increment: &(Score, Local)) -> bool {
        self.timezone.today().0 - increment.1.day().0 < DAYS_STORED as _
    }
}

#[derive(Default)]
pub struct SegmentedScoreIncrements {
    pub outdated: Vec<(Score, Local)>,
    pub valid: Vec<(Score, Local)>,
}

pub fn convert_to_days_offset(input: Vec<(Score, Local)>, timezone: Timezone) -> Vec<(Score, DaysOffset)> {
    input
        .iter()
        .map(|increment| {
            let days_offset = DaysOffset::try_from(timezone.today().0 - increment.1.day().0)
                .unwrap_or_else(|_| panic_str("Failed to calculate days offset"));
            (increment.0, days_offset)
        })
        .collect()
}

impl From<AccountScoreLegacy> for AccountScore {
    fn from(value: AccountScoreLegacy) -> Self {
        let mut history = [DailyScore::default(); DAYS_STORED];
        for (i, item) in history.iter_mut().enumerate().take(DAYS_STORED) {
            item.value = value.scores_history[i];
        }

        Self {
            updated_at: value.updated_at,
            history,
        }
    }
}
