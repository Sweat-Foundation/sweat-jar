use std::{cell::RefCell, collections::HashMap};

use near_plugins::{access_control, AccessControlRole, AccessControllable, Upgradable};
use near_sdk::{
    borsh::BorshDeserialize, collections::UnorderedMap, env, json_types::Base64VecU8, near, require, store::LookupMap,
    AccountId, BorshStorageKey, PanicOnDefault,
};
use strum::{EnumIter, IntoEnumIterator};
use sweat_jar_model::{
    data::{
        account::versioned::AccountVersioned,
        product::{Product, ProductId},
    },
    TokenAmount,
};

#[cfg(all(feature = "replay-engine", target_arch = "wasm32"))]
compile_error!("replay-engine is a host-only feature and must never reach the contract wasm");

mod common;
mod doc;
mod feature;
mod migration;
#[cfg(any(test, feature = "replay-engine"))]
pub mod replay;

pub const PACKAGE_NAME: &str = env!("CARGO_PKG_NAME");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Roles for `near_plugins`' `AccessControllable`. `StagingManager`/`UpgradeManager`
/// are deliberately separate from `Maintainer`: code-deployment is a more
/// dangerous capability than the operational roles and must not come bundled.
///
/// Must live in the same module as the `#[access_control]`-annotated struct:
/// the `AccessControlRole` derive generates a private `RoleFlags` type that
/// the `access_control` expansion refers to by name.
#[near(serializers = [json])]
#[derive(AccessControlRole, Copy, Clone, Debug, PartialEq, Eq, Hash, EnumIter)]
pub enum Roles {
    Oracle,
    ProductManager,
    FeeManager,
    Maintainer,
    StagingManager,
    UpgradeManager,
}

impl Roles {
    /// All role variants, without consumers needing to import strum's
    /// `IntoEnumIterator` trait.
    pub fn all() -> impl Iterator<Item = Self> {
        Self::iter()
    }
}

/// Initial holders for each role, passed explicitly to `init`.
/// Roles absent from the map get no initial holders; further accounts can
/// always be granted later via the standard `acl_grant_role`.
pub type RoleAssignments = HashMap<Roles, Vec<AccountId>>;

/// Convenience for the common case: one account holding every role.
pub fn all_roles_to(account_id: &AccountId) -> RoleAssignments {
    Roles::all().map(|role| (role, vec![account_id.clone()])).collect()
}

pub trait InitApi {
    fn init(
        token_account_id: AccountId,
        fee_account_id: AccountId,
        previous_version_account_id: AccountId,
        super_admin: AccountId,
        roles: RoleAssignments,
    ) -> Self;
}

/// The `Contract` struct represents the state of the smart contract managing fungible token deposit jars.
///
/// The layout is identical in production and integration-test builds: the
/// integration-test time scale lives under its own raw storage key
/// (see `sweat_jar_model::time_scale`), not in a field here.
#[near(contract_state)]
#[derive(PanicOnDefault, Upgradable)]
#[access_control(role_type(Roles))]
#[upgradable(access_control_roles(
    code_stagers(Roles::StagingManager),
    code_deployers(Roles::UpgradeManager),
    duration_initializers(Roles::UpgradeManager),
    duration_update_stagers(Roles::UpgradeManager),
    duration_update_appliers(Roles::UpgradeManager),
))]
pub struct Contract {
    /// The account ID of the fungible token contract (NEP-141) that this jars contract interacts with.
    pub token_account_id: AccountId,

    /// The account ID where fees for applicable operations are directed.
    pub fee_account_id: AccountId,

    /// A collection of products, each representing terms for specific deposit jars.
    pub products: UnorderedMap<ProductId, Product>,

    /// A lookup map that associates account IDs with sets of jars owned by each account.
    pub accounts: LookupMap<AccountId, AccountVersioned>,

    /// Cache to make access to products faster.
    /// Is not stored in contract state (skipped by borsh).
    #[borsh(skip)]
    pub products_cache: RefCell<HashMap<ProductId, Product>>,

    pub fee_amount: TokenAmount,
    pub previous_version_account_id: AccountId,
}

#[near]
#[derive(BorshStorageKey)]
pub(crate) enum StorageKey {
    Products,
    Accounts,
}

#[near]
impl InitApi for Contract {
    #[init]
    #[private]
    fn init(
        token_account_id: AccountId,
        fee_account_id: AccountId,
        previous_version_account_id: AccountId,
        super_admin: AccountId,
        roles: RoleAssignments,
    ) -> Self {
        let mut contract = Self {
            token_account_id,
            fee_account_id,
            products: UnorderedMap::new(StorageKey::Products),
            products_cache: HashMap::default().into(),
            accounts: LookupMap::new(StorageKey::Accounts),
            fee_amount: 0,
            previous_version_account_id,
        };

        contract.init_authority(super_admin, roles);

        contract
    }
}

// Not `#[near]`-annotated: the macro would force these helpers `pub`.
impl Contract {
    /// One-shot ACL bootstrap for `init`: the predecessor (the contract's own
    /// account, forced by `#[private]`) becomes a temporary super-admin so it
    /// can perform the grants — a fresh ACL has no admins, and
    /// `acl_grant_role`/`acl_transfer_super_admin` authorize by predecessor —
    /// then hands super-admin off to `super_admin`, retaining no power itself.
    /// Every step is `require!`d: a silent ACL failure must abort the whole
    /// transaction, never complete init with a misconfigured ACL.
    pub(crate) fn init_authority(&mut self, super_admin: AccountId, roles: RoleAssignments) {
        require!(
            self.acl_init_super_admin(env::predecessor_account_id()),
            "ACL bootstrap failed: super admin is already initialized"
        );
        self.grant_role_assignments(roles);
        require!(
            self.acl_transfer_super_admin(super_admin).is_some(),
            "ACL bootstrap failed: could not transfer super admin"
        );
    }

    pub(crate) fn grant_role_assignments(&mut self, roles: RoleAssignments) {
        for (role, account_ids) in roles {
            for account_id in account_ids {
                // `None` = predecessor lacks admin permission for the role;
                // `Some(false)` (account already held it) is fine.
                require!(
                    self.acl_grant_role(role.into(), account_id).is_some(),
                    "ACL bootstrap failed: could not grant role"
                );
            }
        }
    }
}
