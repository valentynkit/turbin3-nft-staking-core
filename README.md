# anchor-core-staking

NFT staking on Metaplex Core, written in Anchor. Turbin3 Q3 2026, week 5.

Built on top of the instructor's base program,
[AndreiaCanadas/anchor-core-staking](https://github.com/AndreiaCanadas/anchor-core-staking).
The base already had `initialize`, `create_collection`, `mint_asset`, `stake` and `unstake`.
Staking freezes the asset with a FreezeDelegate plugin and records `staked` / `staked_at` in its
Attributes plugin; unstaking checks the freeze period, thaws the asset and mints reward tokens.

## What I added

- `claim_rewards`: mint the rewards earned so far while the NFT stays staked and frozen.
- `burn_staked_nft`: burn a staked NFT through a BurnDelegate held by the program and get
  the accrued rewards plus a one-time bonus.
- A `total_staked` counter on the collection itself (Attributes plugin on the collection).
- A time-based transfer oracle: assets in the collection can only change hands between
  09:00 and 17:00 UTC. Includes a permissionless crank (`update_oracle`) that pays a small
  SOL reward for flipping the state on time, and a `transfer_nft` instruction.
- Rust integration tests on LiteSVM with a controllable clock, replacing the TypeScript /
  surfpool tests.

Smaller changes to the base:

- `unstake` now removes the FreezeDelegate and BurnDelegate plugins after thawing. In the base
  the FreezeDelegate stayed on the asset, so staking the same NFT a second time failed with
  "plugin already exists".
- Rewards accrue from a new `claimed_at` attribute. `staked_at` keeps the original stake time,
  so claiming does not restart the freeze period.

## Instructions

| Instruction | Who signs | What it does |
|---|---|---|
| `create_collection(name, uri)` | payer, collection keypair | Creates the Core collection with the update authority PDA, a `total_staked = 0` attribute and the Oracle adapter for transfers |
| `initialize(rewards_bps, freeze_period)` | admin | Creates the config PDA and the rewards mint (6 decimals, config PDA is mint authority) |
| `mint_asset(name, uri)` | user, asset keypair | Mints a Core asset into the collection |
| `stake` | owner | Writes `staked`, `staked_at`, `claimed_at`, freezes the asset, adds the BurnDelegate, `total_staked += 1` |
| `claim_rewards` | owner | Pays full days since `claimed_at`, moves `claimed_at` forward by those days. Fails with `NothingToClaim` if less than a day has passed |
| `unstake` | owner | Requires `freeze_period` days since `staked_at`, pays days since `claimed_at`, thaws, removes both delegates, `total_staked -= 1` |
| `burn_staked_nft` | owner | Requires `freeze_period` days, pays days since `claimed_at` plus 100 tokens, burns the asset, `total_staked -= 1` |
| `init_oracle` | payer | Creates the oracle PDA with the state for the current time |
| `update_oracle` | anyone | Crank. Sets Transfer to Approved or Rejected from the clock, pays the caller when the rule below allows it |
| `transfer_nft` | owner | Core `TransferV1` CPI with the oracle PDA as a remaining account |

Rewards are `days * rewards_bps / 10000` whole tokens per NFT, counted in full days. With
`rewards_bps = 10000` that is one token per day.

## Accounts and PDAs

| Account | Seeds | Notes |
|---|---|---|
| Update authority | `["update_authority", collection]` | Collection update authority, signs every authority-managed plugin change and the burn |
| Config | `["config", collection]` | `rewards_bps`, `freeze_period`, bumps |
| Rewards mint | `["rewards_mint", config]` | SPL token, 6 decimals |
| Oracle | `["oracle"]` | One for the whole program. Anchor account whose data after the discriminator is Core's `OracleValidation` |
| Oracle vault | `["oracle_vault"]` | Plain system account holding the crank rewards. Fund it with a normal SOL transfer |
| Collection | keypair | Core collection with the Attributes plugin and the Oracle adapter |
| Asset | keypair | Core asset with Attributes, FreezeDelegate and BurnDelegate while staked |

## Burning with BurnDelegate

At stake time the owner signs an `AddPluginV1` that adds a BurnDelegate whose authority is
`PluginAuthority::Address { address: update_authority_pda }`. BurnDelegate is an owner-managed
plugin, which is why it has to be added in `stake`, where the owner is already signing.

`burn_staked_nft` then does two CPIs signed by that PDA:

1. Thaw: set FreezeDelegate `frozen = false`. A frozen asset rejects a burn even when a
   BurnDelegate approves it, so this has to happen first.
2. `BurnV1` with the PDA as authority. Core sees that the signer matches the BurnDelegate
   authority and approves.

Core does not delete the account. It shrinks it to one byte (`Key::Uninitialized`) and sends the
rest of the rent to the payer, so the address can never be used for a new asset. The owner still
has to sign `burn_staked_nft`; the PDA only signs inside the program.

`unstake` removes the BurnDelegate again, so the program can only burn assets that are currently
staked. While staked, the owner cannot remove the plugin (a frozen asset rejects plugin removal),
but Core does let them revoke the delegate's authority. That only takes away their own burn
option, since `burn_staked_nft` then fails for that asset.

## Collection counter

`create_collection` adds an Attributes plugin to the collection with one entry,
`total_staked = "0"`, managed by the update authority. `stake`, `unstake` and `burn_staked_nft`
each read it with `fetch_plugin`, add `+1` or `-1`, and write the list back with
`UpdateCollectionPluginV1` signed by the update authority PDA (`utils::update_total_staked`).
Any other attributes on the collection are kept as they are.

## Transfer oracle

Core's Oracle adapter reads a validation result from an account you point it at. The collection
is created with:

- `base_address` = oracle PDA, no `base_address_config`
- `results_offset = Anchor`, so Core skips the 8 byte discriminator
- one lifecycle check: `Transfer` with the reject flag only (`flags = 4`)

Because the adapter sits on the collection, it applies to every asset in it, including transfers
that go straight to Core without this program.

**Window.** `09:00 <= unix_timestamp % 86400 < 17:00` means Approved, anything else Rejected.
Create, burn and update are always `Pass`.

**Crank rule.** `update_oracle` is permissionless:

- If the computed state equals the stored one, it returns without writing or paying anything.
  Repeating the call cannot drain the vault.
- If the state changed, it is written. The caller gets 0.001 SOL from the vault only when the
  call lands within 15 minutes after the boundary that caused the flip (09:00 for opening,
  17:00 for closing). A late flip still updates the oracle but earns nothing.
- If the vault would drop below rent exemption, the flip still happens and nothing is paid.

Each boundary can pay at most once, because only the first call after it changes the state.

**Why reject only.** In Core an external plugin that returns Approved with the approve
capability counts as an approval, the same way an owner signature does. If the oracle could
approve, any wallet could move any asset during the open hours. With only the reject flag,
Core ignores Approved and the oracle can only veto. The owner (or a real delegate) still has to
authorize every transfer. The tests check this: inside the window a direct Core transfer signed
by a stranger fails with `NoApprovals`.

**transfer_nft.** Core looks up the oracle account among the instruction's accounts, so
`transfer_nft` passes the oracle PDA as a remaining account on the `TransferV1` CPI. Outside the
window Core returns `InvalidAuthority`.

Things to know: the oracle follows the crank, not the clock. If nobody cranks at 17:00,
transfers stay open until someone does. Staked assets cannot be transferred at any time
because they are frozen.

## Running it

Prerequisites (one time):

- Rust (stable) for the host test build
- Solana CLI 4.1.2 (`agave-install init 4.1.2`)
- Anchor CLI 1.2.1 (`avm install 1.2.1`)

Both versions are pinned in `Anchor.toml` under `[toolchain]`.

Then:

```sh
anchor test
```

`anchor test` builds the program to `target/deploy/anchor_core_staking.so` and runs
`cargo test -p anchor-core-staking --test integration`. No validator is started. The tests load
the program and `tests/fixtures/mpl_core.so` into LiteSVM and set the `Clock` sysvar directly,
so days of staking and the 09:00 / 17:00 boundaries take no real time.

After a build you can also run the tests on their own:

```sh
cargo test -p anchor-core-staking --test integration
```

The mpl-core fixture is the mainnet program, dumped with:

```sh
solana program dump -u m CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d tests/fixtures/mpl_core.so
```

The full output of the last run is in [docs/test-output.txt](docs/test-output.txt).

![tests passing](docs/tests-passing.png)

## Version notes

- `anchor-lang` stays at 0.31.1 because mpl-core 0.11's `anchor` feature requires it. The 1.2.1
  CLI builds it fine and prints a version mismatch warning on every build, which is expected.
- Anchor CLI 1.x builds SBPF v3. LiteSVM 0.10 cannot load that, so the tests use LiteSVM 0.16.
- The base repo's surfpool runbooks under `runbooks/` are left as they were for deployment.
  The TypeScript tests, `package.json`, `yarn.lock` and the migration script were removed.

## Known limitations

- `mint_asset` is open to anyone, as in the base. Combined with staking rewards and the burn
  bonus, anyone can mint many NFTs and farm tokens after the freeze period. Fixing that needs a
  mint gate or a capped supply, which is outside this assignment.
- There is one oracle for the whole program, shared by every collection it creates.
