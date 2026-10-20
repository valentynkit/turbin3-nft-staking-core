use anchor_lang::prelude::*;

use crate::constants::{SECONDS_PER_DAY, TRANSFER_WINDOW_CLOSE, TRANSFER_WINDOW_OPEN};

// Mirrors mpl-core's OracleValidation so the Core program can read this account
// directly (results_offset = Anchor skips the 8 byte discriminator)
#[account]
#[derive(InitSpace)]
pub struct Oracle {
    pub validation: OracleValidation,
    pub bump: u8,
    pub vault_bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq, InitSpace)]
pub enum OracleValidation {
    Uninitialized,
    V1 {
        create: ExternalValidationResult,
        transfer: ExternalValidationResult,
        burn: ExternalValidationResult,
        update: ExternalValidationResult,
    },
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum ExternalValidationResult {
    Approved,
    Rejected,
    Pass,
}

impl OracleValidation {
    pub fn for_transfer(transfer: ExternalValidationResult) -> Self {
        OracleValidation::V1 {
            create: ExternalValidationResult::Pass,
            transfer,
            burn: ExternalValidationResult::Pass,
            update: ExternalValidationResult::Pass,
        }
    }
}

// Approved inside the 09:00-17:00 UTC window, Rejected outside
pub fn transfer_result_at(unix_timestamp: i64) -> ExternalValidationResult {
    let seconds_of_day = unix_timestamp.rem_euclid(SECONDS_PER_DAY);
    if (TRANSFER_WINDOW_OPEN..TRANSFER_WINDOW_CLOSE).contains(&seconds_of_day) {
        ExternalValidationResult::Approved
    } else {
        ExternalValidationResult::Rejected
    }
}
