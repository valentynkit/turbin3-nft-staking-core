use crate::constants::SECONDS_PER_DAY;
use crate::error::ErrorCode;
use crate::utils::{full_days_between, mint_rewards, read_stake_info, staking_attributes};
use crate::Config;
use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token_interface::{Mint, TokenAccount, TokenInterface},
};
use mpl_core::{
    accounts::{BaseAssetV1, BaseCollectionV1},
    instructions::UpdatePluginV1CpiBuilder,
    types::{Attributes, Plugin, UpdateAuthority},
    ID as MPL_CORE_ID,
};

#[derive(Accounts)]
pub struct ClaimRewards<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(
        seeds = [b"config", collection.key().as_ref()],
        bump = config.bump,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        has_one = owner @ ErrorCode::InvalidOwner,
        constraint = asset.update_authority == UpdateAuthority::Collection(collection.key()) @ ErrorCode::InvalidUpdateAuthority,
    )]
    pub asset: Account<'info, BaseAssetV1>,
    #[account(
        mut,
        has_one = update_authority @ ErrorCode::InvalidUpdateAuthority
    )]
    pub collection: Account<'info, BaseCollectionV1>,
    /// CHECK: This account data is not used, we only verify the address
    #[account(
        seeds = [b"update_authority", collection.key().as_ref()],
        bump,
    )]
    pub update_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [b"rewards_mint", config.key().as_ref()],
        bump = config.rewards_bump,
    )]
    pub rewards_mint: InterfaceAccount<'info, Mint>,
    #[account(
        init_if_needed,
        payer = owner,
        associated_token::mint = rewards_mint,
        associated_token::authority = owner,
    )]
    pub user_rewards_ata: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    /// CHECK: This is the MPL Core program
    #[account(address = Pubkey::from(MPL_CORE_ID.to_bytes()))]
    pub mpl_core_program: UncheckedAccount<'info>,
}
pub fn handler(ctx: Context<ClaimRewards>) -> Result<()> {
    let stake_info = read_stake_info(&ctx.accounts.asset.to_account_info())?;
    let now = Clock::get()?.unix_timestamp;

    let reward_days = full_days_between(stake_info.claimed_at, now)?;
    require!(reward_days > 0, ErrorCode::NothingToClaim);

    // Move claimed_at forward by whole days only, so the partial day in progress keeps accruing
    let claimed_at = stake_info
        .claimed_at
        .checked_add(reward_days as i64 * SECONDS_PER_DAY)
        .ok_or(ErrorCode::Overflow)?;

    let collection_key = ctx.accounts.collection.key();
    let signer_seeds = &[
        b"update_authority",
        collection_key.as_ref(),
        &[ctx.bumps.update_authority],
    ];

    // The asset stays staked and frozen, only claimed_at changes
    UpdatePluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
        .asset(&ctx.accounts.asset.to_account_info())
        .collection(Some(&ctx.accounts.collection.to_account_info()))
        .payer(&ctx.accounts.owner.to_account_info())
        .authority(Some(&ctx.accounts.update_authority.to_account_info()))
        .system_program(&ctx.accounts.system_program.to_account_info())
        .plugin(Plugin::Attributes(Attributes {
            attribute_list: staking_attributes(
                stake_info.other_attributes,
                true,
                stake_info.staked_at,
                claimed_at,
            ),
        }))
        .invoke_signed(&[signer_seeds])?;

    let amount = ctx
        .accounts
        .config
        .rewards_for_days(reward_days, ctx.accounts.rewards_mint.decimals)?;

    mint_rewards(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.rewards_mint,
        &ctx.accounts.user_rewards_ata.to_account_info(),
        &ctx.accounts.config,
        collection_key,
        amount,
    )
}
