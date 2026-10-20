use crate::constants::{
    CRANK_REWARD_LAMPORTS, CRANK_REWARD_WINDOW, SECONDS_PER_DAY, TRANSFER_WINDOW_CLOSE,
    TRANSFER_WINDOW_OPEN,
};
use crate::state::{transfer_result_at, ExternalValidationResult, Oracle, OracleValidation};
use anchor_lang::prelude::*;
use anchor_lang::system_program::{transfer, Transfer};

#[derive(Accounts)]
pub struct UpdateOracle<'info> {
    #[account(mut)]
    pub cranker: Signer<'info>,
    #[account(
        mut,
        seeds = [b"oracle"],
        bump = oracle.bump,
    )]
    pub oracle: Account<'info, Oracle>,
    #[account(
        mut,
        seeds = [b"oracle_vault"],
        bump = oracle.vault_bump,
    )]
    pub vault: SystemAccount<'info>,
    pub system_program: Program<'info, System>,
}
pub fn handler(ctx: Context<UpdateOracle>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let transfer_result = transfer_result_at(now);
    let validation = OracleValidation::for_transfer(transfer_result);

    // Nothing changed: no write and no reward, so repeated calls cannot drain the vault
    if ctx.accounts.oracle.validation == validation {
        return Ok(());
    }
    ctx.accounts.oracle.validation = validation;

    // Pay only for flips made shortly after the boundary that caused them. A late flip
    // still updates the oracle, it just earns nothing
    let boundary = if transfer_result == ExternalValidationResult::Approved {
        TRANSFER_WINDOW_OPEN
    } else {
        TRANSFER_WINDOW_CLOSE
    };
    let since_boundary = (now.rem_euclid(SECONDS_PER_DAY) - boundary).rem_euclid(SECONDS_PER_DAY);
    if since_boundary >= CRANK_REWARD_WINDOW {
        return Ok(());
    }

    // Keep the vault rent exempt; an empty vault just means no reward
    let min_balance = Rent::get()?.minimum_balance(0);
    if ctx.accounts.vault.lamports() < CRANK_REWARD_LAMPORTS + min_balance {
        return Ok(());
    }

    let vault_seeds: &[&[u8]] = &[b"oracle_vault", &[ctx.accounts.oracle.vault_bump]];
    transfer(
        CpiContext::new_with_signer(
            ctx.accounts.system_program.to_account_info(),
            Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.cranker.to_account_info(),
            },
            &[vault_seeds],
        ),
        CRANK_REWARD_LAMPORTS,
    )
}
