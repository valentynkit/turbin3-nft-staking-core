use anchor_lang::prelude::*;
use anchor_spl::token_interface::{mint_to_checked, Mint, MintToChecked};
use mpl_core::{
    accounts::{BaseAssetV1, BaseCollectionV1},
    fetch_plugin,
    instructions::UpdateCollectionPluginV1CpiBuilder,
    types::{Attribute, Attributes, Plugin, PluginType},
};

use crate::constants::SECONDS_PER_DAY;
use crate::error::ErrorCode;
use crate::state::Config;

// Staking attributes of a staked asset, plus every other attribute so they can be written back untouched
pub struct StakeInfo {
    pub staked_at: i64,
    pub claimed_at: i64,
    pub other_attributes: Vec<Attribute>,
}

pub fn read_stake_info(asset: &AccountInfo) -> Result<StakeInfo> {
    let (_, attributes, _) = fetch_plugin::<BaseAssetV1, Attributes>(asset, PluginType::Attributes)
        .map_err(|_| ErrorCode::AssetNotStaked)?;

    let mut staked = false;
    let mut staked_at = None;
    let mut claimed_at = None;
    let mut other_attributes = Vec::with_capacity(attributes.attribute_list.len());

    for attribute in attributes.attribute_list {
        match attribute.key.as_str() {
            "staked" => staked = attribute.value == "true",
            "staked_at" => staked_at = Some(parse_timestamp(&attribute.value)?),
            "claimed_at" => claimed_at = Some(parse_timestamp(&attribute.value)?),
            _ => other_attributes.push(attribute),
        }
    }

    require!(staked, ErrorCode::AssetNotStaked);
    let staked_at = staked_at.ok_or(ErrorCode::InvalidTimestamp)?;

    Ok(StakeInfo {
        staked_at,
        // Assets staked before claim_rewards existed have no claimed_at yet
        claimed_at: claimed_at.unwrap_or(staked_at),
        other_attributes,
    })
}

pub fn staking_attributes(
    mut attributes: Vec<Attribute>,
    staked: bool,
    staked_at: i64,
    claimed_at: i64,
) -> Vec<Attribute> {
    attributes.push(Attribute {
        key: "staked".to_string(),
        value: staked.to_string(),
    });
    attributes.push(Attribute {
        key: "staked_at".to_string(),
        value: staked_at.to_string(),
    });
    attributes.push(Attribute {
        key: "claimed_at".to_string(),
        value: claimed_at.to_string(),
    });
    attributes
}

pub fn full_days_between(from: i64, to: i64) -> Result<u64> {
    let elapsed = to.checked_sub(from).ok_or(ErrorCode::InvalidTimestamp)?;
    require!(elapsed >= 0, ErrorCode::InvalidTimestamp);
    Ok((elapsed / SECONDS_PER_DAY) as u64)
}

fn parse_timestamp(value: &str) -> Result<i64> {
    value
        .parse::<i64>()
        .map_err(|_| ErrorCode::InvalidTimestamp.into())
}

// Adds `delta` to the "total_staked" attribute of the collection's Attributes plugin.
// The plugin is authority-managed, so the update authority PDA signs.
pub fn update_total_staked<'info>(
    collection: &AccountInfo<'info>,
    update_authority: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    mpl_core_program: &AccountInfo<'info>,
    signer_seeds: &[&[u8]],
    delta: i64,
) -> Result<()> {
    let (_, mut attributes, _) =
        fetch_plugin::<BaseCollectionV1, Attributes>(collection, PluginType::Attributes)
            .map_err(|_| ErrorCode::MissingCollectionStats)?;

    let total_staked = attributes
        .attribute_list
        .iter_mut()
        .find(|attribute| attribute.key == "total_staked")
        .ok_or(ErrorCode::MissingCollectionStats)?;
    let total = total_staked
        .value
        .parse::<u64>()
        .map_err(|_| ErrorCode::MissingCollectionStats)?
        .checked_add_signed(delta)
        .ok_or(ErrorCode::Overflow)?;
    total_staked.value = total.to_string();

    UpdateCollectionPluginV1CpiBuilder::new(mpl_core_program)
        .collection(collection)
        .payer(payer)
        .authority(Some(update_authority))
        .system_program(system_program)
        .plugin(Plugin::Attributes(attributes))
        .invoke_signed(&[signer_seeds])?;

    Ok(())
}

// Mints reward tokens to `to`. The config PDA is the mint authority
pub fn mint_rewards<'info>(
    token_program: &AccountInfo<'info>,
    rewards_mint: &InterfaceAccount<'info, Mint>,
    to: &AccountInfo<'info>,
    config: &Account<'info, Config>,
    collection_key: Pubkey,
    amount: u64,
) -> Result<()> {
    let config_seeds = &[b"config", collection_key.as_ref(), &[config.bump]];
    let config_signer_seeds = &[&config_seeds[..]];

    mint_to_checked(
        CpiContext::new_with_signer(
            token_program.clone(),
            MintToChecked {
                mint: rewards_mint.to_account_info(),
                to: to.clone(),
                authority: config.to_account_info(),
            },
            config_signer_seeds,
        ),
        amount,
        rewards_mint.decimals,
    )
}
