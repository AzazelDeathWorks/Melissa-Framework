//! One host budget for both conversation and agent dispatch. Wire-token
//! observations are deliberately not accepted as native input counts.
use crate::{GatewayError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TokenBudget {
    pub capacity_tokens: u64,
    pub reserved_output_tokens: u64,
    pub input_budget_tokens: u64,
    pub conservative_input_units: u64,
    pub exact_input_tokens: Option<u64>,
    pub remaining_input_units: Option<u64>,
    pub basis: String,
}
impl TokenBudget {
    pub fn assess(
        native_json_bytes: u64,
        messages: usize,
        exact_input_tokens: Option<u64>,
        capacity_tokens: u64,
        reserved_output_tokens: u64,
    ) -> Result<Self> {
        let input_budget_tokens = capacity_tokens
            .checked_sub(reserved_output_tokens)
            .filter(|_| capacity_tokens > 0)
            .ok_or_else(|| GatewayError::invalid("output reservation exceeds context capacity"))?;
        let framing = u64::try_from(messages)
            .ok()
            .and_then(|n| n.checked_mul(64))
            .and_then(|n| n.checked_add(1024))
            .ok_or_else(|| GatewayError::invalid("context framing overflow"))?;
        let conservative_input_units = native_json_bytes
            .checked_add(framing)
            .ok_or_else(|| GatewayError::invalid("context byte bound overflow"))?;
        let input = exact_input_tokens.unwrap_or(conservative_input_units);
        Ok(Self {
            capacity_tokens,
            reserved_output_tokens,
            input_budget_tokens,
            conservative_input_units,
            exact_input_tokens,
            remaining_input_units: input_budget_tokens.checked_sub(input),
            basis: if exact_input_tokens.is_some() {
                "trusted_native_counter"
            } else {
                "conservative_native_bytes_plus_framing; not measured tokens"
            }
            .into(),
        })
    }
    pub fn input_units(&self) -> u64 {
        self.exact_input_tokens
            .unwrap_or(self.conservative_input_units)
    }
    pub fn fits(&self) -> bool {
        self.remaining_input_units.is_some()
    }
    pub fn should_compact(&self) -> bool {
        u128::from(self.input_units()) * 100 >= u128::from(self.input_budget_tokens) * 80
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_reservation_is_never_reduced_and_unknown_is_not_zero() {
        let b = TokenBudget::assess(1000, 2, None, 8192, 4096).unwrap();
        assert_eq!(b.conservative_input_units, 2152);
        assert_eq!(b.exact_input_tokens, None);
        assert_eq!(b.remaining_input_units, Some(1944));
        assert!(b.fits());
        let full = TokenBudget::assess(1000, 2, None, 2200, 49).unwrap();
        assert!(!full.fits());
        assert!(full.should_compact());
        assert_eq!(full.reserved_output_tokens, 49);
    }
    #[test]
    fn only_explicit_native_count_can_replace_the_conservative_bound() {
        let b = TokenBudget::assess(10_000, 2, Some(0), 8192, 4096).unwrap();
        assert_eq!(b.exact_input_tokens, Some(0));
        assert!(b.fits());
        assert_eq!(b.remaining_input_units, Some(4096));
        assert!(
            !TokenBudget::assess(10_000, 2, None, 8192, 4096)
                .unwrap()
                .fits()
        );
        assert!(
            !TokenBudget::assess(1, 1, Some(u64::MAX), 8192, 4096)
                .unwrap()
                .fits()
        );
    }
    #[test]
    fn compaction_uses_the_input_budget_not_the_whole_context() {
        let b = TokenBudget::assess(1, 1, Some(80), 200, 100).unwrap();
        assert!(b.should_compact());
        assert!(
            !TokenBudget::assess(1, 1, Some(79), 200, 100)
                .unwrap()
                .should_compact()
        );
        assert!(TokenBudget::assess(u64::MAX, 1, None, 8192, 1).is_err());
        assert!(TokenBudget::assess(1, 1, None, 10, 11).is_err());
        assert!(TokenBudget::assess(1, 1, None, 0, 0).is_err());
    }
}
