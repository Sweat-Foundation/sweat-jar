use near_plugins::{access_control_any, AccessControllable};
use near_sdk::{env, json_types::U64, near, require};
use sweat_jar_model::{api::SunsetApi, sunset};

use crate::{
    common::event::{emit, EventKind},
    Contract, ContractExt, Roles,
};

#[near]
impl SunsetApi for Contract {
    fn get_sunset_at(&self) -> Option<U64> {
        sunset::sunset_at().map(Into::into)
    }

    #[access_control_any(roles(Roles::UpgradeManager))]
    fn set_sunset_at(&mut self, sunset_at: Option<U64>) {
        if let Some(sunset_at) = sunset_at {
            require!(sunset_at.0 > env::block_timestamp_ms(), "Sunset must be in the future");
        }

        sunset::set_sunset_at(sunset_at.map(|value| value.0));

        emit(EventKind::SetSunset(sunset_at));
    }
}
