use anchor_lang::prelude::*;
use mpl_core::{
    instructions::CreateCollectionV2CpiBuilder,
    types::{
        Attribute, Attributes, ExternalCheckResult, ExternalPluginAdapterInitInfo,
        HookableLifecycleEvent, OracleInitInfo, Plugin, PluginAuthority, PluginAuthorityPair,
        ValidationResultsOffset,
    },
    ID as MPL_CORE_ID,
};

// ExternalCheckResult flags: 1 = listen, 2 = approve, 4 = reject
const CAN_REJECT: u32 = 4;

#[derive(Accounts)]
pub struct CreateCollection<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut)]
    pub collection: Signer<'info>,
    /// CHECK: This account is not initialized and is being used for signing purposes only, we verify that derives from the correct seeds
    #[account(
        seeds = [b"update_authority", collection.key().as_ref()],
        bump,
    )]
    pub update_authority: UncheckedAccount<'info>,
    /// CHECK: Only the address is stored in the collection's Oracle adapter, it does not need to exist yet
    #[account(
        seeds = [b"oracle"],
        bump,
    )]
    pub oracle: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
    /// CHECK: This is the ID of the MPL Core Program
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,
}
pub fn handler(ctx: Context<CreateCollection>, name: String, uri: String) -> Result<()> {
    // Signer seeds for the update authority
    let collection_key = ctx.accounts.collection.key();
    let signer_seeds = &[
        b"update_authority",
        collection_key.as_ref(),
        &[ctx.bumps.update_authority],
    ];

    CreateCollectionV2CpiBuilder::new(&ctx.accounts.mpl_core_program.to_account_info())
        .collection(&ctx.accounts.collection.to_account_info())
        .payer(&ctx.accounts.payer.to_account_info())
        .update_authority(Some(&ctx.accounts.update_authority.to_account_info()))
        .system_program(&ctx.accounts.system_program.to_account_info())
        .name(name)
        .uri(uri)
        // Collection-level stats, kept up to date by stake / unstake / burn_staked_nft
        .plugins(vec![PluginAuthorityPair {
            plugin: Plugin::Attributes(Attributes {
                attribute_list: vec![Attribute {
                    key: "total_staked".to_string(),
                    value: "0".to_string(),
                }],
            }),
            authority: Some(PluginAuthority::UpdateAuthority),
        }])
        // Transfer gate: Core reads the oracle PDA on every transfer of an asset in this collection.
        // Reject-only, so the oracle can veto a transfer but can never approve one on its own
        .external_plugin_adapters(vec![ExternalPluginAdapterInitInfo::Oracle(
            OracleInitInfo {
                base_address: ctx.accounts.oracle.key(),
                init_plugin_authority: Some(PluginAuthority::UpdateAuthority),
                lifecycle_checks: vec![(
                    HookableLifecycleEvent::Transfer,
                    ExternalCheckResult { flags: CAN_REJECT },
                )],
                base_address_config: None,
                results_offset: Some(ValidationResultsOffset::Anchor),
            },
        )])
        .invoke_signed(&[signer_seeds])?;

    Ok(())
}
