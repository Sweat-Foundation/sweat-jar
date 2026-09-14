//! Amount parser and replay-window constants.

/// Snapshot block time (ms): start of the replay window.
pub const H_MS: u64 = 1_774_017_710_156;
/// Snapshot block height on NEAR mainnet (the `H_MS` block).
pub const H_BLOCK: u64 = 190_375_496;
/// End of the replay window (ms).
pub const T_END_MS: u64 = 1_788_174_657_961;

/// Decimal string of yocto -> `u128`. Trims whitespace. Errors on a decimal point or non-digits.
pub fn yocto_str_to_u128(s: &str) -> anyhow::Result<u128> {
    s.trim()
        .parse::<u128>()
        .map_err(|e| anyhow::anyhow!("invalid yocto amount {s:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yocto() {
        assert_eq!(
            yocto_str_to_u128(" 500000000000000000000000 ").unwrap(),
            500_000_000_000_000_000_000_000
        );
        assert!(yocto_str_to_u128("12.5").is_err());
    }
}
