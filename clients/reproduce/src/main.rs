//! Runs one price vault round against devnet (or a local validator) and reads every result
//! back from the chain: M3-002's S1 and S2, M3-004's S1 and S2, and two refusals as signed
//! simulations.
//!
//! It fetches real signed price updates from Pyth's Hermes service, posts them through the
//! Pyth receiver program, and settles the round on the one update that covers its expiry.
//!
//! Environment:
//! - `DEVNET_PAYER_KEYPAIR`: path to a funded devnet keypair file. Read, never printed.
//! - `PYTH_API_KEY`: Hermes API key. Sent as a bearer token, never printed.
//! - `SOLANA_RPC_URL`: optional, defaults to the public devnet endpoint. Only its host is
//!   printed, because the address may carry a key.
//! - `HERMES_URL`: optional, defaults to `https://hermes.pyth.network`.

mod chain;
mod hermes;

use {
    anchor_lang::{
        prelude::Pubkey,
        solana_program::instruction::{AccountMeta, Instruction},
        system_program, AccountDeserialize, InstructionData, Space, ToAccountMetas,
    },
    chain::Chain,
    price_vault::{
        error::VaultError,
        state::{
            Outcome, Position, Round, Side, Vault, MIN_LOCK_SECS, POSITION_SEED, ROUND_SEED,
            VAULT_SEED,
        },
    },
    pyth_solana_receiver_sdk::price_update::{PriceUpdateV2, VerificationLevel},
    solana_keypair::Keypair,
    solana_signer::Signer,
    std::{error::Error, process::ExitCode, thread::sleep, time::Duration},
};

/// SOL/USD.
const FEED_HEX: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
const FEED_NAME: &str = "SOL/USD";

const STAKE_UP: u64 = 10_000_000; // 0.01 SOL
const STAKE_DOWN: u64 = 5_000_000; // 0.005 SOL

/// Seconds from opening the round to its join deadline. Each transaction is finalized
/// before the next, so this has to cover funding two players and two joins.
const JOIN_WINDOW_SECS: i64 = 90;

type Res<T> = Result<T, Box<dyn Error>>;

/// Counts the checks made against values read back from the chain.
#[derive(Default)]
struct Checks {
    passed: u32,
    failed: u32,
}

impl Checks {
    fn check(&mut self, ok: bool, what: &str) {
        if ok {
            self.passed += 1;
            println!("   PASS  {what}");
        } else {
            self.failed += 1;
            println!("   FAIL  {what}");
        }
    }
}

fn sol(lamports: u64) -> String {
    format!(
        "{}.{:09} SOL",
        lamports / 1_000_000_000,
        lamports % 1_000_000_000
    )
}

/// A Pyth price as a decimal string: `price * 10^exponent`.
fn decimal(value: i128, exponent: i32) -> String {
    if exponent >= 0 {
        return format!("{}", value * 10i128.pow(exponent as u32));
    }
    let scale = 10i128.pow(exponent.unsigned_abs());
    let sign = if value < 0 { "-" } else { "" };
    let value = value.abs();
    format!(
        "{sign}{}.{:0width$}",
        value / scale,
        value % scale,
        width = exponent.unsigned_abs() as usize
    )
}

/// `HH:MM:SS UTC` for a unix time. The date is left out: a run is minutes long.
fn clock(unix: i64) -> String {
    let s = unix.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02} UTC", s / 3600, s % 3600 / 60, s % 60)
}

fn round_pda(opener: &Pubkey, round_id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[ROUND_SEED, opener.as_ref(), &round_id.to_le_bytes()],
        &price_vault::id(),
    )
    .0
}

fn vault_pda(round: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[VAULT_SEED, round.as_ref()], &price_vault::id()).0
}

fn position_pda(round: &Pubkey, player: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[POSITION_SEED, round.as_ref(), player.as_ref()],
        &price_vault::id(),
    )
    .0
}

fn vault_ix(data: impl InstructionData, accounts: impl ToAccountMetas) -> Instruction {
    Instruction::new_with_bytes(
        price_vault::id(),
        &data.data(),
        accounts.to_account_metas(None),
    )
}

fn settle_ix(round: Pubkey, price_update: Pubkey) -> Instruction {
    vault_ix(
        price_vault::instruction::Settle {},
        price_vault::accounts::Settle {
            round,
            price_update,
        },
    )
}

fn transfer_ix(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    // The system program's Transfer: instruction index 2, then the amount.
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction::new_with_bytes(
        system_program::ID,
        &data,
        vec![AccountMeta::new(*from, true), AccountMeta::new(*to, false)],
    )
}

fn round_state(chain: &Chain, round: &Pubkey) -> Res<Round> {
    let account = chain
        .account(round)?
        .ok_or("the round account is missing")?;
    Ok(Round::try_deserialize(&mut account.data.as_slice())?)
}

/// Post one Hermes update through the Pyth receiver and check what it left on chain.
fn post_price(
    chain: &Chain,
    checks: &mut Checks,
    payer: &Keypair,
    update: &hermes::Update,
) -> Res<Pubkey> {
    let (message_account, price_account) = (Keypair::new(), Keypair::new());
    let (verify, post) = hermes::post_update_ixs(
        chain,
        &payer.pubkey(),
        &message_account.pubkey(),
        &price_account.pubkey(),
        update,
    )?;
    // The second transaction can only succeed after the first, so the first is waited for
    // at `confirmed` and checked at `finalized` once the second is. Waiting for both in
    // turn would use up half of the 60 seconds an entry price stays fresh.
    let verified = chain.send_confirmed(
        "have Pyth's Wormhole program verify the signed update",
        &verify,
        payer,
        &[&message_account],
    )?;
    chain.send(
        "post the verified price through the Pyth receiver",
        &post,
        payer,
        &[&price_account],
    )?;
    chain.expect_finalized(&verified)?;

    let account = chain
        .account(&price_account.pubkey())?
        .ok_or("the price update account is missing")?;
    let posted = PriceUpdateV2::try_deserialize(&mut account.data.as_slice())?;
    checks.check(
        account.owner == pyth_solana_receiver_sdk::ID,
        "the price update account is owned by the Pyth receiver program",
    );
    checks.check(
        posted.verification_level == VerificationLevel::Full,
        "the receiver verified the update in full",
    );
    checks.check(
        posted.price_message.price == update.price
            && posted.price_message.conf == update.conf
            && posted.price_message.publish_time == update.publish_time,
        "the price on chain is the price Hermes reported",
    );
    Ok(price_account.pubkey())
}

/// Sleep until the cluster's own clock reaches `moment`.
fn wait_until(chain: &Chain, moment: i64) -> Res<()> {
    loop {
        let now = chain.unix_time()?;
        if now >= moment {
            return Ok(());
        }
        println!("   chain time {}, {} s to go", clock(now), moment - now);
        sleep(Duration::from_secs(15.min((moment - now) as u64).max(2)));
    }
}

fn run() -> Res<Checks> {
    let mut checks = Checks::default();
    let feed_id: [u8; 32] = hex::decode(FEED_HEX)?
        .try_into()
        .map_err(|_| "the feed id is not 32 bytes")?;

    let chain = Chain::from_env()?;
    let hermes = hermes::Hermes::from_env()?;
    let payer = chain::read_keypair("DEVNET_PAYER_KEYPAIR")?;

    println!("Price vault: one round, opened, joined, locked and settled from Pyth, and paid out");
    println!();
    println!("Network     {}", chain.describe());
    println!("Program     {}", price_vault::id());
    println!("Payer       {}", payer.pubkey());
    let balance_before = chain.balance(&payer.pubkey())?;
    println!("Balance     {}", sol(balance_before));
    println!("Feed        {FEED_NAME} ({FEED_HEX})");
    println!();

    let program = chain
        .account(&price_vault::id())?
        .ok_or("the program is not deployed on this cluster")?;
    checks.check(program.executable, "the program account is executable");

    // ---- 1. open ------------------------------------------------------------------------
    println!();
    println!("1. Open a round. It has no entry price yet");
    let now = chain.unix_time()?;
    let round_id = now as u64;
    let join_deadline = now + JOIN_WINDOW_SECS;
    let expiry = join_deadline + MIN_LOCK_SECS;
    let round = round_pda(&payer.pubkey(), round_id);
    let vault = vault_pda(&round);
    println!("   round       {round}");
    println!("   vault       {vault}");
    println!(
        "   schedule    now {}, positions close {}, expiry {}",
        clock(now),
        clock(join_deadline),
        clock(expiry)
    );
    chain.send(
        "open_round",
        &[vault_ix(
            price_vault::instruction::OpenRound {
                round_id,
                feed_id,
                join_deadline,
                expiry,
            },
            price_vault::accounts::OpenRound {
                opener: payer.pubkey(),
                round,
                vault,
                system_program: system_program::ID,
            },
        )],
        &payer,
        &[],
    )?;
    let state = round_state(&chain, &round)?;
    checks.check(
        state.feed_id == feed_id && state.join_deadline == join_deadline && state.expiry == expiry,
        "the round records the feed, the join deadline and the expiry",
    );
    let vault_rent = chain.balance(&vault)?;
    checks.check(
        state.outcome == Outcome::Open && vault_rent == chain.rent(8 + Vault::INIT_SPACE)?,
        "the round is open and its vault holds only its rent",
    );

    // ---- 2. join (S1) -------------------------------------------------------------------
    println!();
    println!("2. Two throwaway players join: one up, one down (S1)");
    let (up, down) = (Keypair::new(), Keypair::new());
    let position_rent = chain.rent(8 + Position::INIT_SPACE)?;
    println!("   up player   {} stakes {}", up.pubkey(), sol(STAKE_UP));
    println!(
        "   down player {} stakes {}",
        down.pubkey(),
        sol(STAKE_DOWN)
    );
    chain.send(
        "fund both players with their stake and their position's rent",
        &[
            transfer_ix(&payer.pubkey(), &up.pubkey(), STAKE_UP + position_rent),
            transfer_ix(&payer.pubkey(), &down.pubkey(), STAKE_DOWN + position_rent),
        ],
        &payer,
        &[],
    )?;
    for (player, side, stake, name) in [
        (&up, Side::Up, STAKE_UP, "join up"),
        (&down, Side::Down, STAKE_DOWN, "join down"),
    ] {
        chain.send(
            name,
            &[vault_ix(
                price_vault::instruction::Join {
                    side,
                    amount: stake,
                },
                price_vault::accounts::Join {
                    player: player.pubkey(),
                    round,
                    vault,
                    position: position_pda(&round, &player.pubkey()),
                    system_program: system_program::ID,
                },
            )],
            &payer,
            &[player],
        )?;
    }
    let pool = STAKE_UP + STAKE_DOWN;
    checks.check(
        chain.balance(&vault)? == vault_rent + pool,
        "the vault holds both stakes",
    );
    for (player, side, stake, name) in [
        (&up, Side::Up, STAKE_UP, "up"),
        (&down, Side::Down, STAKE_DOWN, "down"),
    ] {
        let account = chain
            .account(&position_pda(&round, &player.pubkey()))?
            .ok_or("a position account is missing")?;
        let position = Position::try_deserialize(&mut account.data.as_slice())?;
        checks.check(
            position.player == player.pubkey()
                && position.round == round
                && position.side == side
                && position.amount == stake,
            &format!("the {name} position records its player, side and amount"),
        );
    }
    let state = round_state(&chain, &round)?;
    checks.check(
        !state.locked && state.entry_price == 0,
        "both players chose a side while the round had no entry price",
    );

    // ---- 3. lock: the entry price -------------------------------------------------------
    println!();
    println!(
        "3. Wait for positions to close, {}, then fix the entry price",
        clock(join_deadline)
    );
    wait_until(&chain, join_deadline)?;
    let entry = hermes.at(FEED_HEX, join_deadline)?;
    println!(
        "   Hermes      {} +/- {} published {} (previous update {})",
        decimal(entry.price.into(), entry.exponent),
        decimal(entry.conf.into(), entry.exponent),
        clock(entry.publish_time),
        clock(entry.prev_publish_time)
    );
    checks.check(
        entry.prev_publish_time < join_deadline && join_deadline <= entry.publish_time,
        "Hermes returned the one update that covers the join deadline",
    );
    let entry_account = post_price(&chain, &mut checks, &payer, &entry)?;
    chain.send(
        "lock_round",
        &[vault_ix(
            price_vault::instruction::LockRound {},
            price_vault::accounts::LockRound {
                round,
                price_update: entry_account,
            },
        )],
        &payer,
        &[],
    )?;
    let state = round_state(&chain, &round)?;
    checks.check(
        state.locked
            && state.entry_price == entry.price
            && state.entry_publish_time == entry.publish_time,
        "the round's entry price is that update's price",
    );

    // ---- 4. a refusal before the expiry (S3) --------------------------------------------
    println!();
    println!("4. Settle before the expiry is refused (S3), shown by simulation: nothing is sent");
    let watched = [round, vault];
    let before = chain.accounts(&watched)?;
    let simulated = chain.simulate(&[settle_ix(round, entry_account)], &payer)?;
    checks.check(
        simulated.custom_error == Some(6000 + VaultError::SettleTooEarly as u32)
            && simulated.logs_name("SettleTooEarly"),
        "the simulation fails with SettleTooEarly",
    );
    checks.check(
        chain.accounts(&watched)? == before,
        "the round and the vault are unchanged",
    );

    // ---- 5. wait ------------------------------------------------------------------------
    println!();
    println!("5. Wait for the expiry, {}", clock(expiry));
    wait_until(&chain, expiry)?;

    // ---- 6. settle (S2) -----------------------------------------------------------------
    println!();
    println!("6. Settle on the update that covers the expiry (S2)");
    let before = chain.accounts(&watched)?;
    let simulated = chain.simulate(&[settle_ix(round, entry_account)], &payer)?;
    checks.check(
        simulated.custom_error == Some(6000 + VaultError::PriceTooEarly as u32)
            && simulated.logs_name("PriceTooEarly"),
        "settling with the entry price, now too old, fails with PriceTooEarly (simulated)",
    );
    checks.check(
        chain.accounts(&watched)? == before,
        "the round and the vault are unchanged",
    );

    let exit = hermes.at(FEED_HEX, expiry)?;
    println!(
        "   Hermes      {} +/- {} published {} (previous update {})",
        decimal(exit.price.into(), exit.exponent),
        decimal(exit.conf.into(), exit.exponent),
        clock(exit.publish_time),
        clock(exit.prev_publish_time)
    );
    checks.check(
        exit.prev_publish_time < expiry && expiry <= exit.publish_time,
        "Hermes returned the one update that covers the expiry",
    );
    let exit_account = post_price(&chain, &mut checks, &payer, &exit)?;
    chain.send("settle", &[settle_ix(round, exit_account)], &payer, &[])?;

    let state = round_state(&chain, &round)?;
    let expected = match exit.price.cmp(&entry.price) {
        std::cmp::Ordering::Greater => Outcome::Up,
        std::cmp::Ordering::Less => Outcome::Down,
        std::cmp::Ordering::Equal => Outcome::Refund,
    };
    println!(
        "   result      entry {}, exit {}: {:?}",
        decimal(entry.price.into(), entry.exponent),
        decimal(exit.price.into(), exit.exponent),
        state.outcome
    );
    checks.check(
        state.outcome == expected && state.exit_price == exit.price,
        "the round's result follows from the two prices",
    );
    checks.check(
        chain.balance(&vault)? == vault_rent + pool,
        "settling moved nothing: the pool is still in the vault",
    );

    // ---- 7. claim (S2) ------------------------------------------------------------------
    println!();
    println!("7. Pay both positions (S2)");
    for (player, name) in [
        (&up, "claim for the up player"),
        (&down, "claim for the down player"),
    ] {
        chain.send(
            name,
            &[vault_ix(
                price_vault::instruction::Claim {},
                price_vault::accounts::Claim {
                    round,
                    vault,
                    position: position_pda(&round, &player.pubkey()),
                    player: player.pubkey(),
                },
            )],
            &payer,
            &[],
        )?;
    }
    // Each player also gets their position's rent back when it closes.
    let (up_payout, down_payout) = match state.outcome {
        Outcome::Up => (pool, 0),
        Outcome::Down => (0, pool),
        _ => (STAKE_UP, STAKE_DOWN),
    };
    let up_balance = chain.balance(&up.pubkey())?;
    let down_balance = chain.balance(&down.pubkey())?;
    println!(
        "   up player   received {}",
        sol(up_balance - position_rent)
    );
    println!(
        "   down player received {}",
        sol(down_balance - position_rent)
    );
    checks.check(
        up_balance == up_payout + position_rent && down_balance == down_payout + position_rent,
        match state.outcome {
            Outcome::Refund => "a tie: each player has their own stake back",
            _ => "the winner has the whole pool and the loser has nothing",
        },
    );
    checks.check(
        chain.balance(&vault)? == vault_rent,
        "the vault holds no stake",
    );
    checks.check(
        chain
            .account(&position_pda(&round, &up.pubkey()))?
            .is_none()
            && chain
                .account(&position_pda(&round, &down.pubkey()))?
                .is_none(),
        "both positions are closed",
    );

    // ---- 8. close and tidy up -----------------------------------------------------------
    println!();
    println!("8. Close the round and return everything to the payer");
    let mut tidy = vec![vault_ix(
        price_vault::instruction::CloseRound {},
        price_vault::accounts::CloseRound {
            opener: payer.pubkey(),
            round,
            vault,
        },
    )];
    for price_account in [entry_account, exit_account] {
        tidy.push(hermes::reclaim_rent_ix(&payer.pubkey(), &price_account));
    }
    tidy.push(transfer_ix(&up.pubkey(), &payer.pubkey(), up_balance));
    tidy.push(transfer_ix(&down.pubkey(), &payer.pubkey(), down_balance));
    chain.send(
        "close_round, reclaim the two price accounts' rent, sweep both players",
        &tidy,
        &payer,
        &[&up, &down],
    )?;
    checks.check(
        chain.account(&round)?.is_none() && chain.account(&vault)?.is_none(),
        "the round and its vault are gone",
    );
    checks.check(
        chain.balance(&up.pubkey())? == 0 && chain.balance(&down.pubkey())? == 0,
        "the throwaway players hold nothing",
    );

    let balance_after = chain.balance(&payer.pubkey())?;
    println!();
    println!("Payer balance before  {}", sol(balance_before));
    println!("Payer balance after   {}", sol(balance_after));
    println!(
        "Spent                 {} (transaction fees and Pyth's update fees)",
        sol(balance_before.saturating_sub(balance_after))
    );
    println!("Transactions sent     {}", chain.sent());
    Ok(checks)
}

fn main() -> ExitCode {
    match run() {
        Ok(checks) if checks.failed == 0 => {
            println!();
            println!("All {} checks passed.", checks.passed);
            ExitCode::SUCCESS
        }
        Ok(checks) => {
            println!();
            println!(
                "{} of {} checks FAILED.",
                checks.failed,
                checks.failed + checks.passed
            );
            ExitCode::FAILURE
        }
        Err(error) => {
            println!();
            println!("Stopped: {}", chain::redact(&error.to_string()));
            ExitCode::FAILURE
        }
    }
}
