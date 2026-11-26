# Price-settled vault on Solana

An Anchor program where two or more players stake SOL on whether a price will be higher or lower a few minutes from now. Nobody decides who won: the round starts and ends on signed prices from [Pyth](https://www.pyth.network/), and the program checks each one before it uses it. It runs on Solana devnet with play money. It has not been audited and must not hold real funds.

This is Deed #8 of The Solana Deeds, a series of one-day learning builds.

It has a [write-up](https://reddi.tech/blog/solana-m3-pyth-vault) and a [demo](https://youtu.be/ALrnFGwP5iw), about four minutes.

This is a source snapshot of `6e2f435513d45905df8c8f4f582ad9997b84f951`. The private build journal, issue history, evidence files and hosted checks are not part of this repository. Test names and a few comments carry labels such as `M3-002` or `M3-004 S1`: those are outcome and scenario numbers from that private history. `docs/SECURITY.md`, named in one test file's opening comment, is that repository's list of required checks: signer, account owner, derived addresses and arithmetic overflow. Built and reviewed by an AI agent session directed by the author; no independent review.

## What was built

A round is a bet on one Pyth price feed between two moments.

| Step | Instruction | Who can call it | What it does |
|---|---|---|---|
| 1 | `open_round` | the opener, who signs | Fixes the feed, the join deadline and the expiry. It reads no price |
| 2 | `join` | a player, who signs | Moves the stake into the round's vault and records up or down. One position per player |
| 3 | `lock_round` | anyone | After the join deadline, fixes the entry price from the price update that covers the deadline |
| 4 | `settle` | anyone | After the expiry, fixes the result from the price update that covers the expiry |
| 5 | `claim` | anyone, for a named player | Pays that player's position and closes it. The payout can only go to the player who opened it |
| 6 | `close_round` | the opener, who signs | Closes the finished round and returns the opener's rent |

`void_round` (anyone) turns a round that nobody locked or settled within an hour of its expiry into a refund.

- **Nobody knows the entry price when they choose a side.** It is taken after the last player can join, so joining late gives no edge over joining early.
- **The caller cannot choose the price.** `lock_round` and `settle` each accept exactly one update: the first one published at or after their moment (`prev_publish_time < moment <= publish_time`), and only within 60 seconds of it.
- **A price is checked before it is used.** The account must be owned by the Pyth receiver program, fully verified, for the round's feed, positive, and with a confidence interval no wider than 0.5% of the price.
- **A round that cannot be decided is refunded.** A tie, nobody on the winning side, or no acceptable price: every stake goes back.
- **No fee and no house share.** The winning side shares the whole pool in proportion to stake.

## Run it

You need [rustup](https://rustup.rs), the Solana CLI (Agave) 4.4.1 and the Anchor CLI 1.2.2. `rustup` installs Rust 1.100.0 from `rust-toolchain.toml` on first use.

```sh
sh -c "$(curl -sSfL https://release.anza.xyz/v4.4.1/install)"   # Solana CLI (Agave) 4.4.1
cargo install --git https://github.com/solana-foundation/anchor --tag v1.2.2 avm --locked
avm install 1.2.2   # Anchor.toml selects 1.2.2 inside this repository

RUSTUP_TOOLCHAIN=1.100.0 anchor build --ignore-keys
cargo test --locked
```

This builds the program and runs 48 tests: 47 load the built program into [LiteSVM](https://github.com/LiteSVM/litesvm) and run whole rounds and every refusal, and one checks the script below. They make no network call. `RUSTUP_TOOLCHAIN=1.100.0` is there because Anchor generates the program's interface file with whatever `stable` Rust the machine has unless told otherwise. `--ignore-keys` is there because the program's keypair is not in this repository.

**A whole round on devnet, against real Pyth prices:**

```sh
export DEVNET_PAYER_KEYPAIR=/path/to/a-funded-devnet-keypair.json   # about 0.05 SOL is plenty
export PYTH_API_KEY=...                                             # from https://app.pyth.com
cargo run --locked -p reproduce
```

Pyth's prices are handed out, with signatures, by a web service of Pyth's called Hermes; the script fetches them from there. It opens a round on the deployed program, has two throwaway wallets join, waits for the join deadline, posts Pyth's price for that second and locks the round, waits for the expiry, posts that second's price, settles, pays out and closes. It prints each transaction's signature and explorer link before sending, waits for it to be finalized, and prints `PASS` or `FAIL` beside each of 27 checks read back from the chain. Two refusals are shown as simulations, so nothing is sent for them. It takes about three minutes and costs about 0.0001 SOL: the stakes and every deposit come back to the payer. It refuses any network that is not devnet or a validator on your own machine.

| Variable | Needed | What it is |
|---|---|---|
| `DEVNET_PAYER_KEYPAIR` | yes | Path to a funded devnet keypair file. Read, never printed |
| `PYTH_API_KEY` | yes | Hermes has required a key since 26 August 2026 |
| `SOLANA_RPC_URL` | no | Defaults to the public devnet endpoint. Only its host is printed |
| `HERMES_URL` | no | Defaults to `https://hermes.pyth.network` |

The program on devnet is `Br6Fg6ZdQLTKxyr9D82fEfh1a1Rz3DN1bLaGZVuQ38H7`. Its upgrade authority is a devnet-only key, so it can be replaced; it was built from this source when this snapshot was made.

## How it works

- **Accounts.** A round (the feed, the schedule, both prices, both sides' totals, the result), its vault (the stakes, held as lamports in an account the program owns) and one position per player. Each address is derived from the ones above it, so another round's vault or position cannot be passed in.
- **Getting a price on chain** takes two steps. Hermes hands out a price with signatures from Pyth's signers. Pyth's Wormhole program checks the signatures on chain, and only then does Pyth's receiver program write the price into an account. There is a one-step shortcut, but it leaves the price marked as partially verified, and this program refuses those.
- **Paying out.** `settle` fixes the result and moves nothing. Each `claim` pays one position its share of what is left, so the last winner takes exactly the remainder and the vault empties to the lamport.

## Known limitations

- **Devnet only, unaudited.** There is no path here to mainnet.
- **Reproducing on devnet needs a Pyth API key.** The tests need none.
- **One update per moment is Pyth's rule.** When this was built, Hermes served one update a second, each naming the second before it. If Pyth changes that, rounds would stop locking or settling and refund; they would not pay the wrong side.
- **A feed that is not trading at the join deadline or the expiry** has no update near it. Such a round is refunded.
- **The limits are constants** (60 seconds, 0.5%, one hour), the same for every round.
- **Rounding.** A pool that does not divide evenly is rounded down for each claim, and the last winner to claim takes what is left: at most a few lamports more.
- **An unclaimed losing position** keeps the round from closing until someone claims it. Anyone can.
- **Not a verifiable build.** Nothing here lets you prove byte for byte that the deployed program is this source.

## Licence

MIT. See [LICENSE](LICENSE).
