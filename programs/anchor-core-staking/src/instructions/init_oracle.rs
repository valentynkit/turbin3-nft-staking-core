use crate::state::{transfer_result_at, Oracle, OracleValidation};
use anchor_lang::prelude::*;

#[derive(Accounts)]
pub struct InitOracle<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = Oracle::DISCRIMINATOR.len() + Oracle::INIT_SPACE,
        seeds = [b"oracle"],
        bump,
    )]
    pub oracle: Account<'info, Oracle>,
    // Holds the lamports paid to crankers. Anyone can top it up with a plain SOL transfer
    #[account(
        seeds = [b"oracle_vault"],
        bump,
    )]
    pub vault: SystemAccount<'info>,
    pub system_program: Program<'info, System>,
}
pub fn handler(ctx: Context<InitOracle>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    ctx.accounts.oracle.set_inner(Oracle {
        validation: OracleValidation::for_transfer(transfer_result_at(now)),
        bump: ctx.bumps.oracle,
        vault_bump: ctx.bumps.vault,
    });

    Ok(())
}
