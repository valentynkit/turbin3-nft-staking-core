use anchor_lang::prelude::*;

#[constant]
pub const SEED: &str = "anchor";

pub const SECONDS_PER_DAY: i64 = 86400;

// One-time bonus for burning a staked NFT, in whole reward tokens
pub const BURN_BONUS_TOKENS: u64 = 100;

// Transfers are allowed between 09:00 and 17:00 UTC
pub const TRANSFER_WINDOW_OPEN: i64 = 9 * 3600;
pub const TRANSFER_WINDOW_CLOSE: i64 = 17 * 3600;

// The crank pays only within this many seconds after the window opens or closes
pub const CRANK_REWARD_WINDOW: i64 = 15 * 60;
pub const CRANK_REWARD_LAMPORTS: u64 = 1_000_000;
