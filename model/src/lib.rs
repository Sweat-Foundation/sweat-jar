pub mod api;
pub mod data;
pub mod interest;
pub mod signer;
pub mod time_scale;
pub mod types;

pub use data::score::*;
pub use time_scale::*;
pub use types::*;

/// Amount of fungible tokens
pub type TokenAmount = u128;

/// Milliseconds since the Unix epoch (January 1, 1970 (midnight UTC/GMT))
pub type Timestamp = u64;

/// Duration in milliseconds
pub type Duration = u64;

pub const MS_IN_SECOND: u64 = 1000;
pub const MS_IN_MINUTE: u64 = MS_IN_SECOND * 60;
pub const MS_IN_HOUR: u64 = MS_IN_MINUTE * 60;
pub const MS_IN_DAY: u64 = MS_IN_HOUR * 24;
pub const MS_IN_YEAR: u64 = MS_IN_DAY * 365;

/// 2026-12-30 14:00 UTC. Interest stops accruing and user operations are rejected from this moment.
pub const SUNSET_AT: Timestamp = 1_798_639_200_000;
