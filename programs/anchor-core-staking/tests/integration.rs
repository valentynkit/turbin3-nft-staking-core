// LiteSVM integration tests. They load target/deploy/anchor_core_staking.so (run `anchor build`
// first, `anchor test` does it for you) and the mainnet mpl-core program from tests/fixtures.

use anchor_core_staking::{
    error::ErrorCode, ExternalValidationResult, Oracle, OracleValidation, BURN_BONUS_TOKENS,
    CRANK_REWARD_LAMPORTS, SECONDS_PER_DAY,
};
use anchor_lang::{
    prelude::Pubkey as AnchorPubkey, AccountDeserialize, InstructionData, ToAccountMetas,
};
use litesvm::LiteSVM;
use mpl_core::{
    errors::MplCoreError,
    types::{Attribute, ExternalValidationResult as CoreResult, OracleValidation as CoreOracle},
    types::{UpdateAuthority, ValidationResultsOffset},
    Asset, Collection,
};
use solana_clock::Clock;
use solana_instruction::{error::InstructionError, AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

const REWARDS_BPS: u16 = 10_000; // one whole token per staked day
const FREEZE_PERIOD_DAYS: u16 = 7;
const ONE_TOKEN: u64 = 1_000_000; // rewards mint has 6 decimals
const HOUR: i64 = 3600;
// 2026-01-01 00:00:00 UTC
const DAY0: i64 = 1_767_225_600;

fn pk(key: AnchorPubkey) -> Pubkey {
    Pubkey::new_from_array(key.to_bytes())
}

fn ak(key: Pubkey) -> AnchorPubkey {
    AnchorPubkey::new_from_array(key.to_bytes())
}

fn ix(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction {
        program_id: pk(anchor_core_staking::ID),
        accounts: accounts
            .to_account_metas(None)
            .into_iter()
            .map(|meta| AccountMeta {
                pubkey: pk(meta.pubkey),
                is_signer: meta.is_signer,
                is_writable: meta.is_writable,
            })
            .collect(),
        data: data.data(),
    }
}

fn pda(seeds: &[&[u8]]) -> AnchorPubkey {
    AnchorPubkey::find_program_address(seeds, &anchor_core_staking::ID).0
}

fn assert_custom_error(result: Result<(), TransactionError>, expected: u32) {
    match result {
        Err(TransactionError::InstructionError(_, InstructionError::Custom(code))) => {
            assert_eq!(code, expected, "unexpected error code")
        }
        other => panic!("expected custom error {expected}, got {other:?}"),
    }
}

fn assert_program_error(result: Result<(), TransactionError>, expected: ErrorCode) {
    assert_custom_error(result, u32::from(expected));
}

fn attribute(attributes: &[Attribute], key: &str) -> String {
    attributes
        .iter()
        .find(|attribute| attribute.key == key)
        .unwrap_or_else(|| panic!("missing attribute {key}"))
        .value
        .clone()
}

struct Test {
    svm: LiteSVM,
    admin: Keypair,
    collection: Keypair,
    update_authority: AnchorPubkey,
    config: AnchorPubkey,
    rewards_mint: AnchorPubkey,
    oracle: AnchorPubkey,
    vault: AnchorPubkey,
}

impl Test {
    // Collection created and config initialized at DAY0 10:00 UTC
    fn new() -> Self {
        let mut svm = LiteSVM::new();
        let manifest = env!("CARGO_MANIFEST_DIR");
        svm.add_program_from_file(
            pk(anchor_core_staking::ID),
            format!("{manifest}/../../target/deploy/anchor_core_staking.so"),
        )
        .expect("program .so missing, run `anchor build`");
        svm.add_program_from_file(
            pk(mpl_core::ID),
            format!("{manifest}/../../tests/fixtures/mpl_core.so"),
        )
        .unwrap();

        let admin = Keypair::new();
        svm.airdrop(&admin.pubkey(), 100_000_000_000).unwrap();
        let collection = Keypair::new();
        let collection_key = ak(collection.pubkey());
        let update_authority = pda(&[b"update_authority", collection_key.as_ref()]);
        let config = pda(&[b"config", collection_key.as_ref()]);
        let rewards_mint = pda(&[b"rewards_mint", config.as_ref()]);

        let mut test = Test {
            svm,
            admin,
            collection,
            update_authority,
            config,
            rewards_mint,
            oracle: pda(&[b"oracle"]),
            vault: pda(&[b"oracle_vault"]),
        };
        test.set_time(DAY0 + 10 * HOUR);
        test.create_collection().unwrap();
        test.initialize().unwrap();
        test
    }

    fn send(
        &mut self,
        instruction: Instruction,
        signers: &[&Keypair],
    ) -> Result<(), TransactionError> {
        let tx = Transaction::new_signed_with_payer(
            &[instruction],
            Some(&signers[0].pubkey()),
            signers,
            self.svm.latest_blockhash(),
        );
        let result = self
            .svm
            .send_transaction(tx)
            .map(|_| ())
            .map_err(|failed| failed.err);
        // Fresh blockhash so an identical retry is a new transaction, not a duplicate
        self.svm.expire_blockhash();
        result
    }

    fn set_time(&mut self, unix_timestamp: i64) {
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp = unix_timestamp;
        self.svm.set_sysvar(&clock);
    }

    fn now(&self) -> i64 {
        self.svm.get_sysvar::<Clock>().unix_timestamp
    }

    fn advance(&mut self, seconds: i64) {
        let now = self.now();
        self.set_time(now + seconds);
    }

    fn new_user(&mut self) -> Keypair {
        let user = Keypair::new();
        self.svm.airdrop(&user.pubkey(), 10_000_000_000).unwrap();
        user
    }

    fn collection_key(&self) -> AnchorPubkey {
        ak(self.collection.pubkey())
    }

    fn create_collection(&mut self) -> Result<(), TransactionError> {
        let instruction = ix(
            anchor_core_staking::accounts::CreateCollection {
                payer: ak(self.admin.pubkey()),
                collection: self.collection_key(),
                update_authority: self.update_authority,
                oracle: self.oracle,
                system_program: anchor_lang::system_program::ID,
                mpl_core_program: mpl_core::ID,
            },
            anchor_core_staking::instruction::CreateCollection {
                name: "Test Collection".to_string(),
                uri: "https://example.com/collection".to_string(),
            },
        );
        let admin = self.admin.insecure_clone();
        let collection = self.collection.insecure_clone();
        self.send(instruction, &[&admin, &collection])
    }

    fn initialize(&mut self) -> Result<(), TransactionError> {
        let instruction = ix(
            anchor_core_staking::accounts::Initialize {
                admin: ak(self.admin.pubkey()),
                config: self.config,
                collection: self.collection_key(),
                update_authority: self.update_authority,
                rewards_mint: self.rewards_mint,
                system_program: anchor_lang::system_program::ID,
                token_program: anchor_spl::token::ID,
            },
            anchor_core_staking::instruction::Initialize {
                rewards_bps: REWARDS_BPS,
                freeze_period: FREEZE_PERIOD_DAYS,
            },
        );
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin])
    }

    fn mint_asset(&mut self, user: &Keypair) -> Keypair {
        let asset = Keypair::new();
        let instruction = ix(
            anchor_core_staking::accounts::MintAsset {
                user: ak(user.pubkey()),
                asset: ak(asset.pubkey()),
                collection: self.collection_key(),
                update_authority: self.update_authority,
                system_program: anchor_lang::system_program::ID,
                mpl_core_program: mpl_core::ID,
            },
            anchor_core_staking::instruction::MintAsset {
                name: "Test NFT".to_string(),
                uri: "https://example.com/nft".to_string(),
            },
        );
        self.send(instruction, &[user, &asset]).unwrap();
        asset
    }

    fn stake(&mut self, owner: &Keypair, asset: &Keypair) -> Result<(), TransactionError> {
        let instruction = ix(
            anchor_core_staking::accounts::Stake {
                owner: ak(owner.pubkey()),
                config: self.config,
                asset: ak(asset.pubkey()),
                collection: self.collection_key(),
                update_authority: self.update_authority,
                system_program: anchor_lang::system_program::ID,
                mpl_core_program: mpl_core::ID,
            },
            anchor_core_staking::instruction::Stake {},
        );
        self.send(instruction, &[owner])
    }

    // unstake, claim_rewards and burn_staked_nft take the same accounts
    fn rewards_ix(
        &self,
        owner: &Keypair,
        asset: &Keypair,
        data: impl InstructionData,
        kind: RewardsIx,
    ) -> Instruction {
        let owner_key = ak(owner.pubkey());
        let user_rewards_ata = anchor_spl::associated_token::get_associated_token_address(
            &owner_key,
            &self.rewards_mint,
        );
        macro_rules! accounts {
            ($name:ident) => {
                anchor_core_staking::accounts::$name {
                    owner: owner_key,
                    config: self.config,
                    asset: ak(asset.pubkey()),
                    collection: self.collection_key(),
                    update_authority: self.update_authority,
                    rewards_mint: self.rewards_mint,
                    user_rewards_ata,
                    token_program: anchor_spl::token::ID,
                    associated_token_program: anchor_spl::associated_token::ID,
                    system_program: anchor_lang::system_program::ID,
                    mpl_core_program: mpl_core::ID,
                }
            };
        }
        match kind {
            RewardsIx::Unstake => ix(accounts!(Unstake), data),
            RewardsIx::Claim => ix(accounts!(ClaimRewards), data),
            RewardsIx::Burn => ix(accounts!(BurnStakedNft), data),
        }
    }

    fn unstake(&mut self, owner: &Keypair, asset: &Keypair) -> Result<(), TransactionError> {
        let instruction = self.rewards_ix(
            owner,
            asset,
            anchor_core_staking::instruction::Unstake {},
            RewardsIx::Unstake,
        );
        self.send(instruction, &[owner])
    }

    fn claim(&mut self, owner: &Keypair, asset: &Keypair) -> Result<(), TransactionError> {
        let instruction = self.rewards_ix(
            owner,
            asset,
            anchor_core_staking::instruction::ClaimRewards {},
            RewardsIx::Claim,
        );
        self.send(instruction, &[owner])
    }

    fn burn(&mut self, owner: &Keypair, asset: &Keypair) -> Result<(), TransactionError> {
        let instruction = self.rewards_ix(
            owner,
            asset,
            anchor_core_staking::instruction::BurnStakedNft {},
            RewardsIx::Burn,
        );
        self.send(instruction, &[owner])
    }

    fn init_oracle(&mut self) -> Result<(), TransactionError> {
        let instruction = ix(
            anchor_core_staking::accounts::InitOracle {
                payer: ak(self.admin.pubkey()),
                oracle: self.oracle,
                vault: self.vault,
                system_program: anchor_lang::system_program::ID,
            },
            anchor_core_staking::instruction::InitOracle {},
        );
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin])
    }

    fn update_oracle(&mut self, cranker: &Keypair) -> Result<(), TransactionError> {
        let instruction = ix(
            anchor_core_staking::accounts::UpdateOracle {
                cranker: ak(cranker.pubkey()),
                oracle: self.oracle,
                vault: self.vault,
                system_program: anchor_lang::system_program::ID,
            },
            anchor_core_staking::instruction::UpdateOracle {},
        );
        self.send(instruction, &[cranker])
    }

    fn transfer_nft(
        &mut self,
        owner: &Keypair,
        asset: &Keypair,
        new_owner: Pubkey,
    ) -> Result<(), TransactionError> {
        let instruction = ix(
            anchor_core_staking::accounts::TransferNft {
                owner: ak(owner.pubkey()),
                asset: ak(asset.pubkey()),
                collection: self.collection_key(),
                new_owner: ak(new_owner),
                oracle: self.oracle,
                system_program: anchor_lang::system_program::ID,
                mpl_core_program: mpl_core::ID,
            },
            anchor_core_staking::instruction::TransferNft {},
        );
        self.send(instruction, &[owner])
    }

    // A plain Core TransferV1, not going through this program
    fn core_transfer_ix(
        &self,
        asset: &Keypair,
        authority: &Keypair,
        new_owner: Pubkey,
    ) -> Instruction {
        let core_ix = mpl_core::instructions::TransferV1Builder::new()
            .asset(ak(asset.pubkey()))
            .collection(Some(self.collection_key()))
            .payer(ak(authority.pubkey()))
            .new_owner(ak(new_owner))
            .add_remaining_account(
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    self.oracle,
                    false,
                ),
            )
            .instruction();
        Instruction {
            program_id: pk(core_ix.program_id),
            accounts: core_ix
                .accounts
                .into_iter()
                .map(|meta| AccountMeta {
                    pubkey: pk(meta.pubkey),
                    is_signer: meta.is_signer,
                    is_writable: meta.is_writable,
                })
                .collect(),
            data: core_ix.data,
        }
    }

    fn lamports(&self, key: AnchorPubkey) -> u64 {
        self.svm
            .get_account(&pk(key))
            .map(|account| account.lamports)
            .unwrap_or(0)
    }

    fn rewards_balance(&self, owner: &Keypair) -> u64 {
        let ata = anchor_spl::associated_token::get_associated_token_address(
            &ak(owner.pubkey()),
            &self.rewards_mint,
        );
        match self.svm.get_account(&pk(ata)) {
            // SPL token account: mint (32) | owner (32) | amount (u64 LE)
            Some(account) => u64::from_le_bytes(account.data[64..72].try_into().unwrap()),
            None => 0,
        }
    }

    fn asset(&self, asset: &Keypair) -> Box<Asset> {
        let account = self.svm.get_account(&asset.pubkey()).unwrap();
        Asset::from_bytes(&account.data).unwrap()
    }

    fn asset_attributes(&self, asset: &Keypair) -> Vec<Attribute> {
        self.asset(asset)
            .plugin_list
            .attributes
            .unwrap()
            .attributes
            .attribute_list
    }

    fn collection(&self) -> Box<Collection> {
        let account = self.svm.get_account(&self.collection.pubkey()).unwrap();
        Collection::from_bytes(&account.data).unwrap()
    }

    fn total_staked(&self) -> u64 {
        let attributes = self
            .collection()
            .plugin_list
            .attributes
            .unwrap()
            .attributes
            .attribute_list;
        attribute(&attributes, "total_staked").parse().unwrap()
    }

    fn oracle_state(&self) -> Oracle {
        let account = self.svm.get_account(&pk(self.oracle)).unwrap();
        Oracle::try_deserialize(&mut account.data.as_slice()).unwrap()
    }

    fn oracle_transfer(&self) -> ExternalValidationResult {
        match self.oracle_state().validation {
            OracleValidation::V1 { transfer, .. } => transfer,
            OracleValidation::Uninitialized => panic!("oracle not initialized"),
        }
    }

    fn fund_vault(&mut self, lamports: u64) {
        let instruction = solana_system_interface::instruction::transfer(
            &self.admin.pubkey(),
            &pk(self.vault),
            lamports,
        );
        let admin = self.admin.insecure_clone();
        self.send(instruction, &[&admin]).unwrap();
    }
}

enum RewardsIx {
    Unstake,
    Claim,
    Burn,
}

// ---------- initialize / create_collection / mint_asset ----------

#[test]
fn initialize_sets_config_and_rewards_mint() {
    let mut t = Test::new();

    let account = t.svm.get_account(&pk(t.config)).unwrap();
    let config =
        anchor_core_staking::Config::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert_eq!(config.rewards_bps, REWARDS_BPS);
    assert_eq!(config.freeze_period, FREEZE_PERIOD_DAYS);

    // SPL mint: mint_authority COption (4 + 32) | supply (8) | decimals (1)
    let mint = t.svm.get_account(&pk(t.rewards_mint)).unwrap();
    assert_eq!(&mint.data[4..36], t.config.as_ref());
    assert_eq!(mint.data[44], 6);

    // The config PDA already exists for this collection
    assert!(t.initialize().is_err());
}

#[test]
fn create_collection_adds_stats_and_oracle_adapter() {
    let t = Test::new();
    let collection = t.collection();

    assert_eq!(collection.base.update_authority, t.update_authority);
    assert_eq!(collection.base.num_minted, 0);

    let attributes = collection.plugin_list.attributes.unwrap();
    assert_eq!(
        attributes.base.authority.authority_type,
        mpl_core::AuthorityType::UpdateAuthority
    );
    assert_eq!(
        attribute(&attributes.attributes.attribute_list, "total_staked"),
        "0"
    );

    let oracles = collection.external_plugin_adapter_list.oracles;
    assert_eq!(oracles.len(), 1);
    assert_eq!(oracles[0].base_address, t.oracle);
    assert_eq!(oracles[0].results_offset, ValidationResultsOffset::Anchor);
}

#[test]
fn mint_asset_creates_asset_in_collection() {
    let mut t = Test::new();
    let user = t.new_user();
    let asset = t.mint_asset(&user);

    let asset = t.asset(&asset);
    assert_eq!(asset.base.owner, ak(user.pubkey()));
    assert_eq!(
        asset.base.update_authority,
        UpdateAuthority::Collection(t.collection_key())
    );
    assert_eq!(t.collection().base.num_minted, 1);
}

// ---------- stake ----------

#[test]
fn stake_freezes_asset_sets_attributes_and_counts() {
    let mut t = Test::new();
    let user = t.new_user();
    let asset = t.mint_asset(&user);
    let staked_at = t.now();

    t.stake(&user, &asset).unwrap();

    let plugins = t.asset(&asset).plugin_list;
    assert!(plugins.freeze_delegate.unwrap().freeze_delegate.frozen);
    assert_eq!(
        plugins.burn_delegate.unwrap().base.authority.address,
        Some(t.update_authority),
        "BurnDelegate authority is the update authority PDA"
    );
    let attributes = t.asset_attributes(&asset);
    assert_eq!(attribute(&attributes, "staked"), "true");
    assert_eq!(attribute(&attributes, "staked_at"), staked_at.to_string());
    assert_eq!(attribute(&attributes, "claimed_at"), staked_at.to_string());
    assert_eq!(t.total_staked(), 1);
}

#[test]
fn stake_rejects_non_owner_and_double_stake() {
    let mut t = Test::new();
    let user = t.new_user();
    let attacker = t.new_user();
    let asset = t.mint_asset(&user);

    assert_program_error(t.stake(&attacker, &asset), ErrorCode::InvalidOwner);

    t.stake(&user, &asset).unwrap();
    assert_program_error(t.stake(&user, &asset), ErrorCode::AlreadyStaked);
    assert_eq!(t.total_staked(), 1);
}

// ---------- claim_rewards ----------

#[test]
fn claim_rewards_pays_full_days_and_keeps_asset_staked() {
    let mut t = Test::new();
    let user = t.new_user();
    let asset = t.mint_asset(&user);
    t.stake(&user, &asset).unwrap();
    let staked_at = t.now();

    // 3 days and 5 hours: 3 full days are paid
    t.advance(3 * SECONDS_PER_DAY + 5 * HOUR);
    t.claim(&user, &asset).unwrap();
    assert_eq!(t.rewards_balance(&user), 3 * ONE_TOKEN);

    // Still owned, staked and frozen; only claimed_at moved, by whole days
    let core_asset = t.asset(&asset);
    assert_eq!(core_asset.base.owner, ak(user.pubkey()));
    assert!(
        core_asset
            .plugin_list
            .freeze_delegate
            .unwrap()
            .freeze_delegate
            .frozen
    );
    let attributes = t.asset_attributes(&asset);
    assert_eq!(attribute(&attributes, "staked"), "true");
    assert_eq!(attribute(&attributes, "staked_at"), staked_at.to_string());
    assert_eq!(
        attribute(&attributes, "claimed_at"),
        (staked_at + 3 * SECONDS_PER_DAY).to_string()
    );
    assert_eq!(t.total_staked(), 1);

    // Claiming again right away has nothing to pay
    assert_program_error(t.claim(&user, &asset), ErrorCode::NothingToClaim);
    assert_eq!(t.rewards_balance(&user), 3 * ONE_TOKEN);

    // The 5 leftover hours were kept: 19 more hours complete day 4
    t.advance(19 * HOUR);
    t.claim(&user, &asset).unwrap();
    assert_eq!(t.rewards_balance(&user), 4 * ONE_TOKEN);
}

#[test]
fn claim_rewards_rejects_non_owner_and_unstaked_asset() {
    let mut t = Test::new();
    let user = t.new_user();
    let attacker = t.new_user();
    let asset = t.mint_asset(&user);

    assert_program_error(t.claim(&user, &asset), ErrorCode::AssetNotStaked);

    t.stake(&user, &asset).unwrap();
    t.advance(2 * SECONDS_PER_DAY);
    assert_program_error(t.claim(&attacker, &asset), ErrorCode::InvalidOwner);
}

// ---------- unstake ----------

#[test]
fn unstake_enforces_freeze_period_from_original_stake() {
    let mut t = Test::new();
    let user = t.new_user();
    let asset = t.mint_asset(&user);
    t.stake(&user, &asset).unwrap();

    t.advance(6 * SECONDS_PER_DAY);
    assert_program_error(t.unstake(&user, &asset), ErrorCode::FreezePeriodNotElapsed);

    // A claim on day 6 does not restart the freeze period
    t.claim(&user, &asset).unwrap();
    assert_eq!(t.rewards_balance(&user), 6 * ONE_TOKEN);

    t.advance(SECONDS_PER_DAY);
    t.unstake(&user, &asset).unwrap();
    // Only the day since the claim is paid on unstake
    assert_eq!(t.rewards_balance(&user), 7 * ONE_TOKEN);
}

#[test]
fn unstake_thaws_removes_delegates_and_decrements() {
    let mut t = Test::new();
    let user = t.new_user();
    let attacker = t.new_user();
    let asset = t.mint_asset(&user);
    t.stake(&user, &asset).unwrap();
    t.advance(8 * SECONDS_PER_DAY);

    assert_program_error(t.unstake(&attacker, &asset), ErrorCode::InvalidOwner);

    t.unstake(&user, &asset).unwrap();
    assert_eq!(t.rewards_balance(&user), 8 * ONE_TOKEN);

    let plugins = t.asset(&asset).plugin_list;
    assert!(
        plugins.freeze_delegate.is_none(),
        "thawed and FreezeDelegate removed"
    );
    assert!(
        plugins.burn_delegate.is_none(),
        "program can no longer burn it"
    );
    let attributes = t.asset_attributes(&asset);
    assert_eq!(attribute(&attributes, "staked"), "false");
    assert_eq!(attribute(&attributes, "staked_at"), "0");
    assert_eq!(t.total_staked(), 0);

    assert_program_error(t.unstake(&user, &asset), ErrorCode::AssetNotStaked);

    // The asset can be staked again
    t.stake(&user, &asset).unwrap();
    assert_eq!(t.total_staked(), 1);
}

// ---------- burn_staked_nft ----------

#[test]
fn burn_staked_nft_pays_bonus_and_burns() {
    let mut t = Test::new();
    let user = t.new_user();
    let attacker = t.new_user();
    let asset = t.mint_asset(&user);
    let other = t.mint_asset(&user);
    t.stake(&user, &asset).unwrap();
    t.stake(&user, &other).unwrap();
    assert_eq!(t.total_staked(), 2);

    t.advance(3 * SECONDS_PER_DAY);
    assert_program_error(t.burn(&user, &asset), ErrorCode::FreezePeriodNotElapsed);

    t.advance(5 * SECONDS_PER_DAY);
    assert_program_error(t.burn(&attacker, &asset), ErrorCode::InvalidOwner);

    t.burn(&user, &asset).unwrap();
    assert_eq!(
        t.rewards_balance(&user),
        8 * ONE_TOKEN + BURN_BONUS_TOKENS * ONE_TOKEN
    );

    // Core leaves a 1 byte tombstone (Key::Uninitialized) so the address cannot be reused
    let burned = t.svm.get_account(&asset.pubkey()).unwrap();
    assert_eq!(burned.data, vec![0]);
    assert_eq!(t.total_staked(), 1);
    assert_eq!(t.collection().base.current_size, 1);

    // The other staked asset is untouched and still counts
    t.unstake(&user, &other).unwrap();
    assert_eq!(t.total_staked(), 0);
}

#[test]
fn burn_staked_nft_rejects_unstaked_asset() {
    let mut t = Test::new();
    let user = t.new_user();
    let asset = t.mint_asset(&user);
    assert_program_error(t.burn(&user, &asset), ErrorCode::AssetNotStaked);
}

// ---------- oracle crank ----------

#[test]
fn oracle_crank_flips_at_boundaries_and_pays_only_near_them() {
    let mut t = Test::new();
    let cranker = t.new_user();

    // 08:00 UTC: closed
    t.set_time(DAY0 + SECONDS_PER_DAY + 8 * HOUR);
    t.init_oracle().unwrap();
    assert_eq!(t.oracle_transfer(), ExternalValidationResult::Rejected);
    t.fund_vault(10 * CRANK_REWARD_LAMPORTS);
    let vault_start = t.lamports(t.vault);

    // 08:59:59: still closed, nothing to do, nothing paid
    t.set_time(DAY0 + SECONDS_PER_DAY + 9 * HOUR - 1);
    t.update_oracle(&cranker).unwrap();
    assert_eq!(t.oracle_transfer(), ExternalValidationResult::Rejected);
    assert_eq!(t.lamports(t.vault), vault_start);

    // 09:00:00: opens, the cranker is paid
    t.set_time(DAY0 + SECONDS_PER_DAY + 9 * HOUR);
    let cranker_before = t.lamports(ak(cranker.pubkey()));
    t.update_oracle(&cranker).unwrap();
    assert_eq!(t.oracle_transfer(), ExternalValidationResult::Approved);
    assert_eq!(t.lamports(t.vault), vault_start - CRANK_REWARD_LAMPORTS);
    assert!(t.lamports(ak(cranker.pubkey())) > cranker_before);

    // 09:05: repeat call is a no-op and pays nothing
    t.set_time(DAY0 + SECONDS_PER_DAY + 9 * HOUR + 5 * 60);
    t.update_oracle(&cranker).unwrap();
    assert_eq!(t.lamports(t.vault), vault_start - CRANK_REWARD_LAMPORTS);

    // 17:30: closes, but 30 minutes late is outside the 15 minute reward window
    t.set_time(DAY0 + SECONDS_PER_DAY + 17 * HOUR + 30 * 60);
    t.update_oracle(&cranker).unwrap();
    assert_eq!(t.oracle_transfer(), ExternalValidationResult::Rejected);
    assert_eq!(t.lamports(t.vault), vault_start - CRANK_REWARD_LAMPORTS);

    // Next day 09:14:59: within the window, paid
    t.set_time(DAY0 + 2 * SECONDS_PER_DAY + 9 * HOUR + 15 * 60 - 1);
    t.update_oracle(&cranker).unwrap();
    assert_eq!(t.oracle_transfer(), ExternalValidationResult::Approved);
    assert_eq!(t.lamports(t.vault), vault_start - 2 * CRANK_REWARD_LAMPORTS);

    // 17:00:00: closes on time, paid
    t.set_time(DAY0 + 2 * SECONDS_PER_DAY + 17 * HOUR);
    t.update_oracle(&cranker).unwrap();
    assert_eq!(t.oracle_transfer(), ExternalValidationResult::Rejected);
    assert_eq!(t.lamports(t.vault), vault_start - 3 * CRANK_REWARD_LAMPORTS);
}

#[test]
fn oracle_crank_still_flips_when_vault_is_empty() {
    let mut t = Test::new();
    let cranker = t.new_user();
    t.set_time(DAY0 + SECONDS_PER_DAY + 8 * HOUR);
    t.init_oracle().unwrap();

    t.set_time(DAY0 + SECONDS_PER_DAY + 9 * HOUR + 60);
    t.update_oracle(&cranker).unwrap();
    assert_eq!(t.oracle_transfer(), ExternalValidationResult::Approved);
    assert_eq!(t.lamports(t.vault), 0);
}

#[test]
fn oracle_account_matches_core_layout() {
    let mut t = Test::new();
    t.init_oracle().unwrap(); // 10:00 UTC, open

    // Core deserializes the bytes after the 8 byte Anchor discriminator as its own OracleValidation
    let account = t.svm.get_account(&pk(t.oracle)).unwrap();
    let core = <CoreOracle as anchor_lang::AnchorDeserialize>::deserialize(&mut &account.data[8..])
        .unwrap();
    assert_eq!(
        core,
        CoreOracle::V1 {
            create: CoreResult::Pass,
            transfer: CoreResult::Approved,
            burn: CoreResult::Pass,
            update: CoreResult::Pass,
        }
    );
}

// ---------- transfer_nft ----------

#[test]
fn transfer_nft_succeeds_inside_window() {
    let mut t = Test::new();
    let user = t.new_user();
    let receiver = t.new_user();
    let attacker = t.new_user();
    let asset = t.mint_asset(&user);

    // 10:00 UTC
    t.init_oracle().unwrap();
    assert_eq!(t.oracle_transfer(), ExternalValidationResult::Approved);

    assert_program_error(
        t.transfer_nft(&attacker, &asset, attacker.pubkey()),
        ErrorCode::InvalidOwner,
    );

    // Approved only means "not rejected": with reject-only checks the oracle never authorizes
    // a stranger, so a direct Core transfer signed by someone else finds no approvals
    let stolen = t.core_transfer_ix(&asset, &attacker, attacker.pubkey());
    assert_custom_error(
        t.send(stolen, &[&attacker]),
        MplCoreError::NoApprovals as u32,
    );

    t.transfer_nft(&user, &asset, receiver.pubkey()).unwrap();
    assert_eq!(t.asset(&asset).base.owner, ak(receiver.pubkey()));
}

#[test]
fn transfer_nft_fails_outside_window() {
    let mut t = Test::new();
    let user = t.new_user();
    let receiver = t.new_user();
    let cranker = t.new_user();
    let asset = t.mint_asset(&user);
    t.init_oracle().unwrap();

    // 18:00 UTC, oracle cranked to Rejected
    t.set_time(DAY0 + 18 * HOUR);
    t.update_oracle(&cranker).unwrap();
    assert_eq!(t.oracle_transfer(), ExternalValidationResult::Rejected);

    assert_custom_error(
        t.transfer_nft(&user, &asset, receiver.pubkey()),
        MplCoreError::InvalidAuthority as u32,
    );
    assert_eq!(t.asset(&asset).base.owner, ak(user.pubkey()));

    // Calling Core directly does not get around the oracle either
    let direct = t.core_transfer_ix(&asset, &user, receiver.pubkey());
    assert_custom_error(
        t.send(direct, &[&user]),
        MplCoreError::InvalidAuthority as u32,
    );

    // Next morning after the crank, the same transfer goes through
    t.set_time(DAY0 + SECONDS_PER_DAY + 9 * HOUR + 60);
    t.update_oracle(&cranker).unwrap();
    t.transfer_nft(&user, &asset, receiver.pubkey()).unwrap();
    assert_eq!(t.asset(&asset).base.owner, ak(receiver.pubkey()));
}

#[test]
fn staked_asset_cannot_be_transferred_even_in_window() {
    let mut t = Test::new();
    let user = t.new_user();
    let receiver = t.new_user();
    let asset = t.mint_asset(&user);
    t.init_oracle().unwrap();
    t.stake(&user, &asset).unwrap();

    assert_custom_error(
        t.transfer_nft(&user, &asset, receiver.pubkey()),
        MplCoreError::InvalidAuthority as u32,
    );
}
