use crate::error::ErrorCode;
use crate::utils::{
    full_days_between, mint_rewards, read_stake_info, staking_attributes, update_total_staked,
};
use crate::Config;
use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token_interface::{Mint, TokenAccount, TokenInterface},
};
use mpl_core::{
    accounts::{BaseAssetV1, BaseCollectionV1},
    instructions::{RemovePluginV1CpiBuilder, UpdatePluginV1CpiBuilder},
    types::{Attributes, FreezeDelegate, Plugin, PluginType, UpdateAuthority},
    ID as MPL_CORE_ID,
};

#[derive(Accounts)]
pub struct Unstake<'info> {
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
pub fn handler(ctx: Context<Unstake>) -> Result<()> {
    // Fails with AssetNotStaked if the staking attributes are missing or "staked" is not "true"
    let stake_info = read_stake_info(&ctx.accounts.asset.to_account_info())?;
    let now = Clock::get()?.unix_timestamp;

    // The freeze period counts from the original stake, rewards from the last claim
    require!(
        full_days_between(stake_info.staked_at, now)? >= ctx.accounts.config.freeze_period as u64,
        ErrorCode::FreezePeriodNotElapsed
    );
    let reward_days = full_days_between(stake_info.claimed_at, now)?;

    // Prepare signing seeds for the update authority
    let collection_key = ctx.accounts.collection.key();
    let signer_seeds = &[
        b"update_authority",
        collection_key.as_ref(),
        &[ctx.bumps.update_authority],
    ];

    // Update the asset Attributes Plugin (keeping any other attributes, staking attributes reset)
    UpdatePluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
        .asset(&ctx.accounts.asset.to_account_info())
        .collection(Some(&ctx.accounts.collection.to_account_info()))
        .payer(&ctx.accounts.owner.to_account_info())
        .authority(Some(&ctx.accounts.update_authority.to_account_info()))
        .system_program(&ctx.accounts.system_program.to_account_info())
        .plugin(Plugin::Attributes(Attributes {
            attribute_list: staking_attributes(stake_info.other_attributes, false, 0, 0),
        }))
        .invoke_signed(&[signer_seeds])?;

    // Thaw the asset (update the FreezeDelegate Plugin to false)
    UpdatePluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
        .asset(&ctx.accounts.asset.to_account_info())
        .collection(Some(&ctx.accounts.collection.to_account_info()))
        .payer(&ctx.accounts.owner.to_account_info())
        .authority(Some(&ctx.accounts.update_authority.to_account_info()))
        .system_program(&ctx.accounts.system_program.to_account_info())
        .plugin(Plugin::FreezeDelegate(FreezeDelegate { frozen: false }))
        .invoke_signed(&[signer_seeds])?;

    // Remove both delegates now that the asset is thawed. The owner gets back an asset the
    // program can no longer burn, and stake can add the plugins again next time
    for plugin_type in [PluginType::BurnDelegate, PluginType::FreezeDelegate] {
        RemovePluginV1CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
            .asset(&ctx.accounts.asset.to_account_info())
            .collection(Some(&ctx.accounts.collection.to_account_info()))
            .payer(&ctx.accounts.owner.to_account_info())
            .authority(Some(&ctx.accounts.owner.to_account_info()))
            .system_program(&ctx.accounts.system_program.to_account_info())
            .plugin_type(plugin_type)
            .invoke()?;
    }

    update_total_staked(
        &ctx.accounts.collection.to_account_info(),
        &ctx.accounts.update_authority.to_account_info(),
        &ctx.accounts.owner.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program.to_account_info(),
        signer_seeds,
        -1,
    )?;

    // Finally, mint the rewards accrued since the last claim
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
