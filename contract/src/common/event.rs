use near_sdk::{
    json_types::{Base64VecU8, U128},
    near, AccountId,
};
#[cfg(not(all(feature = "replay-engine", not(test))))]
use near_sdk::{log, serde_json};
use sweat_jar_model::{
    data::{
        account::features::Feature,
        product::{Product, ProductId},
    },
    Local, Score, Timestamp, TokenAmount, UTC,
};

#[cfg(test)]
use super::env::test_env_ext;
#[cfg(not(all(feature = "replay-engine", not(test))))]
use crate::{env, PACKAGE_NAME, VERSION};

#[derive(Debug, Clone)]
#[near(serializers=[json])]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum EventKind {
    RegisterProduct(Product),
    Deposit(AccountId, DepositData),
    Claim(AccountId, ClaimData),
    Withdraw(AccountId, WithdrawData),
    WithdrawAll(AccountId, Vec<WithdrawData>),
    Restake(AccountId, RestakeData),
    ApplyPenalty(PenaltyData),
    BatchApplyPenalty(BatchPenaltyData),
    EnableProduct(EnableProductData),
    ChangeProductPublicKey(ChangeProductPublicKeyData),
    RecordScore(Vec<ScoreData>),
    OldScoreWarning((Score, Local)),
    JarsMerge(AccountId),
    MigrateProducts(Vec<ProductId>),
    MigrationDisabled,
    MigrationEnabled(AccountId),
    SetFeatureEnabled(AccountId, Feature, bool),
    BatchSetFeatureEnabled(Vec<AccountId>, Feature, bool),
    ApplyBooster(ApplyBoosterData),
}

#[cfg(not(all(feature = "replay-engine", not(test))))]
#[derive(Debug)]
#[near(serializers=[json])]
struct SweatJarEvent {
    standard: &'static str,
    version: &'static str,
    #[serde(flatten)]
    event_kind: EventKind,
}

/// Making a deposit into a Jar.
/// `.0` – ID of a Product describing terms of the Jar.
/// `.1` – amount of tokens to deposit.
pub type DepositData = (ProductId, U128);

/// Claiming interest from a single Jar.
/// `.0` – ID of a Product describing terms of the Jar.
/// `.1` – amount of interest that a User claimed.
pub type ClaimEventItem = (ProductId, U128);

/// Batched claiming interest from a User's account
/// `timestamp` – Unix timestamp of a block where interest was calculated and `ft_transfer` was initiated.
/// `items`     – information about interest claimed from Jars for each Product.
#[derive(Debug, Clone)]
#[near(serializers=[json])]
pub struct ClaimData {
    timestamp: Timestamp,
    pub items: Vec<ClaimEventItem>,
}

impl ClaimData {
    pub fn new(timestamp: Timestamp) -> Self {
        Self {
            timestamp,
            items: Vec::new(),
        }
    }

    pub fn add(&mut self, item: ClaimEventItem) {
        self.items.push(item);
    }
}

/// Withdrawing principal of mature deposits for a single Jar.
/// `.0` – ID of a Product describing terms of the Jar.
/// `.1` – withdrawal fee amount (according to the Product terms).
/// `.2` – amount of withdrawal (minus fee).
/// (id, fee, amount)
pub type WithdrawData = (ProductId, U128, U128);

/// Batched restaking of all User's mature deposits into a single deposit for a particular Product.
/// `timestamp` – Unix timestamp of the operation. In case of partial withdrawal it's time
///               of the initial call.
/// `from`      – a list of Product IDs of deposits sourcing a principal for a new deposit.
/// `into`      – ID of a Product describing terms of the Jar for the new deposit.
/// `restaked`  – amount of tokens being restaked. It's sum of principals of mature deposits
///              for `from` Product IDs minus `withdrawn` amount.
/// `withdrawn` – amount of withdrawn tokens.
#[derive(Debug, Clone)]
#[near(serializers=[json])]
pub struct RestakeData {
    pub timestamp: Timestamp,
    pub from: Vec<ProductId>,
    pub into: ProductId,
    pub restaked: U128,
    pub withdrawn: U128,
    pub is_success: bool,
}

impl RestakeData {
    pub fn new(
        timestamp: Timestamp,
        from: Vec<ProductId>,
        into: ProductId,
        restaked: TokenAmount,
        withdrawn: TokenAmount,
        is_success: bool,
    ) -> Self {
        Self {
            timestamp,
            from,
            into,
            restaked: restaked.into(),
            withdrawn: withdrawn.into(),
            is_success,
        }
    }
}

/// Applying a penalty to a User.
/// `account_id` – ID of an Account that is subject to the penalty.
/// `is_applied` – the penalty is applied or cancelled.
/// `timestamp`  – Unix timestamp of the operation.
#[derive(Debug, Clone)]
#[near(serializers=[json])]
pub struct PenaltyData {
    pub account_id: AccountId,
    pub is_applied: bool,
    pub timestamp: Timestamp,
}

/// Batched applying a penalty to several User.
/// `account_ids` – IDs of Accounts that are subjects to the penalty.
/// `is_applied`  – the penalty is applied or cancelled.
/// `timestamp`   – Unix timestamp of the operation.
#[derive(Debug, Clone)]
#[near(serializers=[json])]
pub struct BatchPenaltyData {
    pub account_ids: Vec<AccountId>,
    pub is_applied: bool,
    pub timestamp: Timestamp,
}

/// Enabling or disabling a Product.
/// `product_id` – ID of affected Product.
/// `is_enabled` – whether the Product became enabled or disabled.
#[derive(Debug, Clone)]
#[near(serializers=[json])]
pub struct EnableProductData {
    pub product_id: ProductId,
    pub is_enabled: bool,
}

/// Change public key for a Product.
/// `product_id` – ID of affected Product.
/// `pk`         – a public key that was set.
#[derive(Debug, Clone)]
#[near(serializers=[json])]
pub struct ChangeProductPublicKeyData {
    pub product_id: ProductId,
    pub pk: Base64VecU8,
}

/// Update of User's score.
/// `account_id` – ID of an Account that is subject to Score update.
/// `score` – a new Score.
#[derive(Debug, Clone)]
#[near(serializers=[json])]
pub struct ScoreData {
    pub account_id: AccountId,
    pub score: Vec<(Score, Local)>,
}

#[derive(Debug, Clone)]
#[near(serializers=[json])]
pub struct ApplyBoosterData {
    pub applied: Vec<AccountId>,
    pub rejected: Vec<AccountId>,
    pub timestamp: UTC,
    pub score: Score,
}

#[cfg(not(all(feature = "replay-engine", not(test))))]
impl From<EventKind> for SweatJarEvent {
    fn from(event_kind: EventKind) -> Self {
        Self {
            standard: PACKAGE_NAME,
            version: VERSION,
            event_kind,
        }
    }
}

#[mutants::skip]
#[cfg(all(not(test), not(feature = "replay-engine")))]
pub(crate) fn emit(event: EventKind) {
    log!("{}", SweatJarEvent::from(event).to_json_event_string());
}

#[mutants::skip]
#[cfg(test)]
pub(crate) fn emit(event: EventKind) {
    test_env_ext::store_event(&event);

    if test_env_ext::should_log_events() {
        log!("{}", SweatJarEvent::from(event).to_json_event_string());
    }
}

#[mutants::skip]
#[cfg(all(feature = "replay-engine", not(test)))]
pub(crate) fn emit(_event: EventKind) {
    // Replay drives millions of users through the contract; event capture
    // (test_env_ext::store_event) would accumulate unboundedly. The replay
    // never inspects emitted events.
}

#[cfg(not(all(feature = "replay-engine", not(test))))]
impl SweatJarEvent {
    fn to_json_string(&self) -> String {
        serde_json::to_string_pretty(self)
            .unwrap_or_else(|err| env::panic_str(&format!("Failed to serialize SweatJarEvent: {err}")))
    }

    fn to_json_event_string(&self) -> String {
        format!("EVENT_JSON:{}", self.to_json_string())
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use near_sdk::{json_types::U128, AccountId};
    use sweat_jar_model::Local;

    use crate::{
        common::{
            event::{ClaimData, EventKind, SweatJarEvent},
            testing::WhitespaceTrimmer,
        },
        VERSION,
    };

    #[test]
    fn event_to_string() {
        let event = SweatJarEvent::from(EventKind::Claim(
            AccountId::from_str("someone.near").unwrap(),
            ClaimData {
                timestamp: 1_234_567,
                items: vec![
                    ("product_0".to_string(), U128(50)),
                    ("product_1".to_string(), U128(200)),
                ],
            },
        ))
        .to_json_event_string();
        let json = r#"EVENT_JSON:{
          "standard": "sweat_jar",
          "version": "{VERSION}",
          "event": "claim",
          "data": [
            "someone.near",
            {
              "timestamp": 1234567,
              "items": [ [ "product_0", "50" ], [ "product_1", "200" ] ]
            }
          ]
        }"#
        .replace("{VERSION}", VERSION);

        assert_eq!(json.trim_whitespaces(), event.trim_whitespaces());

        let event = SweatJarEvent::from(EventKind::OldScoreWarning((111, Local(5)))).to_json_event_string();
        let json = r#"EVENT_JSON:{
          "standard": "sweat_jar",
          "version": "{VERSION}",
          "event": "old_score_warning",
          "data": [ 111, 5 ]
        }"#
        .replace("{VERSION}", VERSION);

        assert_eq!(json.trim_whitespaces(), event.trim_whitespaces());
    }
}
