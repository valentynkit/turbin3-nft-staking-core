use anchor_lang::prelude::*;

use crate::error::ErrorCode;

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub rewards_bps: u16,   // rewards percentage in basis points
    pub freeze_period: u16, // minimum freeze period in days
    pub rewards_bump: u8,   // Bumps for the rewards mint account
    pub bump: u8,           // Bumps for the config account
}

impl Config {
    // Reward tokens (in base units) for a number of full staked days
    pub fn rewards_for_days(&self, days: u64, decimals: u8) -> Result<u64> {
        let amount = days
            .checked_mul(self.rewards_bps as u64)
            .ok_or(ErrorCode::InvalidRewardsBps)?
            .checked_mul(10u64.pow(decimals as u32))
            .ok_or(ErrorCode::InvalidRewardsBps)?
            .checked_div(10000u64)
            .ok_or(ErrorCode::InvalidRewardsBps)?;
        Ok(amount)
    }
}
