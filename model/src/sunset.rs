//! Configurable sunset moment: from it interest stops accruing and user operations are rejected.
//!
//! Persisted under its own raw storage key, outside the `Contract` struct's Borsh layout
//! (same pattern as `time_scale`), so changing it needs no state migration. When the key
//! was never written, jars never close.

use std::cell::Cell;

use near_sdk::{borsh, env};

use crate::Timestamp;

/// Must not collide with near-sdk's `STATE` key, `near_plugins`' `__acl` prefix,
/// `__time_scale`, or the contract's collection prefixes.
const SUNSET_STORAGE_KEY: &[u8] = b"__sunset_at";

#[derive(Clone, Copy)]
enum Cache {
    Unloaded,
    Loaded(Option<Timestamp>),
}

thread_local! {
    /// Per-wasm-instance cache, so repeated reads within one call cost one storage read.
    static CACHED_SUNSET_AT: Cell<Cache> = const { Cell::new(Cache::Unloaded) };
}

/// Current sunset moment, `None` if jars never close.
pub fn sunset_at() -> Option<Timestamp> {
    CACHED_SUNSET_AT.with(|cached| {
        if let Cache::Loaded(value) = cached.get() {
            return value;
        }

        let value = env::storage_read(SUNSET_STORAGE_KEY)
            .and_then(|bytes| borsh::from_slice(&bytes).expect("Persisted sunset must be Option<u64>"));
        cached.set(Cache::Loaded(value));

        value
    })
}

pub fn set_sunset_at(value: Option<Timestamp>) {
    env::storage_write(
        SUNSET_STORAGE_KEY,
        &borsh::to_vec(&value).expect("Failed to serialize sunset"),
    );
    CACHED_SUNSET_AT.with(|cached| cached.set(Cache::Loaded(value)));
}

/// Caps `now` at the sunset moment.
pub fn cap_at_sunset(now: Timestamp) -> Timestamp {
    sunset_at().map_or(now, |sunset| now.min(sunset))
}

pub fn is_sunset(now: Timestamp) -> bool {
    sunset_at().is_some_and(|sunset| now >= sunset)
}

/// Drops the cached value so the next read goes to storage (unit tests only).
#[cfg(feature = "testing")]
pub fn reset_sunset_cache() {
    CACHED_SUNSET_AT.with(|cached| cached.set(Cache::Unloaded));
}
