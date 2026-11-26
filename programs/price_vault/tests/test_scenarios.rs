//! M3-002 scenarios S1 to S3 and M3-004 scenarios S1 to S4 on LiteSVM, and the refund paths
//! that keep a round from locking funds. M3-004 moved the entry price from the moment a
//! round opens to its join deadline; its scenarios are agent-written drafts.

mod common;

use {
    common::*,
    price_vault::{
        error::VaultError,
        state::{
            Outcome, Side, MAX_CONF_BPS, MAX_PRICE_AGE_SECS, MIN_LOCK_SECS, SETTLE_WINDOW_SECS,
        },
    },
    pyth_solana_receiver_sdk::price_update::VerificationLevel,
    solana_signer::Signer,
};

// ---- opening a round ----------------------------------------------------------------------

#[test]
fn open_records_the_feed_and_the_schedule_and_no_entry_price() {
    let mut env = Env::new();
    let round = env.open_default();

    assert_eq!(env.account(&round).unwrap().owner, price_vault::id());
    let state = env.round_state(&round);
    assert_eq!(state.opener, env.opener.pubkey());
    assert_eq!(state.round_id, ROUND_ID);
    assert_eq!(state.feed_id, FEED);
    assert_eq!(state.join_deadline, JOIN_DEADLINE);
    assert_eq!(state.expiry, EXPIRY);
    assert_eq!(state.outcome, Outcome::Open);
    assert_eq!((state.total_up, state.total_down), (0, 0));
    // M3-004: opening reads no price.
    assert!(!state.locked);
    assert_eq!((state.entry_price, state.entry_publish_time), (0, 0));

    // The vault exists, belongs to the program and holds no stake yet.
    assert_eq!(env.account(&env.vault()).unwrap().owner, price_vault::id());
    assert_eq!(env.pool(&round), 0);
}

#[test]
fn open_rejects_a_schedule_that_is_past_out_of_order_or_too_short() {
    let mut env = Env::new();
    let opener = env.opener.insecure_clone();
    let watched = [opener.pubkey(), env.round(), env.vault()];
    let before = env.snapshot(&watched);

    // (join deadline, expiry): deadline not in the future; deadline after the expiry;
    // deadline one second too close to the expiry.
    for (join_deadline, expiry) in [
        (T0, EXPIRY),
        (EXPIRY + 1, EXPIRY),
        (EXPIRY - MIN_LOCK_SECS + 1, EXPIRY),
    ] {
        let ix = env.open_ix(&opener.pubkey(), ROUND_ID, FEED, join_deadline, expiry);
        assert_custom_error(env.send(ix, &[&opener]), code(VaultError::InvalidSchedule));
    }
    assert_eq!(env.snapshot(&watched), before);

    // A deadline exactly the lock period before the expiry is accepted.
    let ix = env.open_ix(
        &opener.pubkey(),
        ROUND_ID,
        FEED,
        EXPIRY - MIN_LOCK_SECS,
        EXPIRY,
    );
    env.send(ix, &[&opener])
        .expect("the shortest round is accepted");
}

// ---- S1: join -----------------------------------------------------------------------------

#[test]
fn s1_join_moves_the_stake_to_the_vault_and_records_the_side_and_amount() {
    let mut env = Env::new();
    let round = env.open_default();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    let alice_before = env.lamports(&alice.pubkey());
    let bob_before = env.lamports(&bob.pubkey());

    env.join(&round, &alice, Side::Up, 3 * SOL);
    env.join(&round, &bob, Side::Down, 2 * SOL);

    // The stakes are in the vault, and each player paid the stake plus their position's rent.
    assert_eq!(env.pool(&round), 5 * SOL);
    assert_eq!(
        env.lamports(&alice.pubkey()),
        alice_before - 3 * SOL - env.position_rent()
    );
    assert_eq!(
        env.lamports(&bob.pubkey()),
        bob_before - 2 * SOL - env.position_rent()
    );

    // Each position is owned by the program and records its round, player, side and amount.
    let position = env.position_state(&round, &alice.pubkey());
    assert_eq!(position.round, round);
    assert_eq!(position.player, alice.pubkey());
    assert_eq!(position.side, Side::Up);
    assert_eq!(position.amount, 3 * SOL);
    let position = env.position_state(&round, &bob.pubkey());
    assert_eq!(position.side, Side::Down);
    assert_eq!(position.amount, 2 * SOL);
    assert_eq!(
        env.account(&position_pda(&round, &alice.pubkey()))
            .unwrap()
            .owner,
        price_vault::id()
    );

    let state = env.round_state(&round);
    assert_eq!((state.total_up, state.total_down), (3 * SOL, 2 * SOL));
    assert_eq!(state.open_positions, 2);
    // M3-004 S1: while players can still join, the round has no entry price to compare with.
    assert!(!state.locked);
    assert_eq!(state.entry_price, 0);
}

#[test]
fn s1_join_rejects_a_zero_stake() {
    let mut env = Env::new();
    let round = env.open_default();
    let alice = env.alice.insecure_clone();
    let watched = [
        alice.pubkey(),
        round,
        env.vault(),
        position_pda(&round, &alice.pubkey()),
    ];
    let before = env.snapshot(&watched);

    let ix = env.join_ix(&round, &alice.pubkey(), Side::Up, 0);
    assert_custom_error(env.send(ix, &[&alice]), code(VaultError::ZeroStake));
    assert_eq!(env.snapshot(&watched), before);
}

#[test]
fn s1_join_is_refused_from_the_join_deadline_on() {
    let mut env = Env::new();
    let round = env.open_default();
    let alice = env.alice.insecure_clone();
    let watched = [
        alice.pubkey(),
        round,
        env.vault(),
        position_pda(&round, &alice.pubkey()),
    ];
    let before = env.snapshot(&watched);

    // One second before the deadline is still open; the deadline itself is not.
    env.set_time(JOIN_DEADLINE);
    let ix = env.join_ix(&round, &alice.pubkey(), Side::Up, SOL);
    assert_custom_error(env.send(ix, &[&alice]), code(VaultError::JoinClosed));
    assert_eq!(env.snapshot(&watched), before);

    env.set_time(JOIN_DEADLINE - 1);
    env.join(&round, &alice, Side::Up, SOL);
    assert_eq!(env.pool(&round), SOL);
}

#[test]
fn s1_a_player_cannot_join_the_same_round_twice() {
    let mut env = Env::new();
    let round = env.open_default();
    let alice = env.alice.insecure_clone();
    env.join(&round, &alice, Side::Up, SOL);
    let watched = [
        alice.pubkey(),
        round,
        env.vault(),
        position_pda(&round, &alice.pubkey()),
    ];
    let before = env.snapshot(&watched);

    // Neither adding to the position nor switching sides: the position account exists.
    for side in [Side::Up, Side::Down] {
        let ix = env.join_ix(&round, &alice.pubkey(), side, SOL);
        assert_custom_error(env.send(ix, &[&alice]), SYSTEM_ACCOUNT_ALREADY_IN_USE);
    }
    assert_eq!(env.snapshot(&watched), before);
}

// ---- S2: settle and pay -------------------------------------------------------------------

#[test]
fn s2_up_wins_the_whole_pool_pro_rata_down_gets_nothing_and_the_vault_ends_empty() {
    let mut env = Env::new();
    let round = env.open_default();
    let (alice, bob, carol) = (
        env.alice.insecure_clone(),
        env.bob.insecure_clone(),
        env.carol.insecure_clone(),
    );
    let opener = env.opener.insecure_clone();
    // The opener's balance before the round existed: 100 SOL from the fixture.
    let opener_start = 100 * SOL;
    let start: Vec<u64> = [&alice, &bob, &carol]
        .iter()
        .map(|k| env.lamports(&k.pubkey()))
        .collect();

    env.join(&round, &alice, Side::Up, 3 * SOL);
    env.join(&round, &bob, Side::Up, SOL);
    env.join(&round, &carol, Side::Down, 2 * SOL);

    // The price ends one cent above where it started.
    env.settle_at(ENTRY_PRICE + 1_000_000);
    let state = env.round_state(&round);
    assert_eq!(state.outcome, Outcome::Up);
    // M3-004 S2: the entry price is the update that covered the join deadline.
    assert!(state.locked);
    assert_eq!(state.entry_price, ENTRY_PRICE);
    assert_eq!(state.entry_publish_time, JOIN_DEADLINE);
    assert_eq!(state.exit_price, ENTRY_PRICE + 1_000_000);
    assert_eq!(state.exit_publish_time, EXPIRY);
    // Settling moves nothing: the pool is still in the vault.
    assert_eq!(env.pool(&round), 6 * SOL);

    for player in [&alice, &bob, &carol] {
        env.claim(&round, &player.pubkey());
    }

    // The pool of 6 SOL is split 3:1 between the up side. The down side receives nothing.
    // Each player's position rent came back when the position closed.
    assert_eq!(
        env.lamports(&alice.pubkey()),
        start[0] - 3 * SOL + 4_500_000_000
    );
    assert_eq!(env.lamports(&bob.pubkey()), start[1] - SOL + 1_500_000_000);
    assert_eq!(env.lamports(&carol.pubkey()), start[2] - 2 * SOL);
    for player in [&alice, &bob, &carol] {
        assert!(env
            .account(&position_pda(&round, &player.pubkey()))
            .is_none());
    }

    // The vault holds no stake: only the rent the opener put in.
    assert_eq!(env.pool(&round), 0);
    assert_eq!(env.lamports(&env.vault()), env.vault_rent());
    let state = env.round_state(&round);
    assert_eq!((state.remaining_pool, state.open_positions), (0, 0));

    // Closing the round removes the vault and the round and returns the opener's rent.
    let ix = env.close_ix(&round, &opener.pubkey());
    env.send(ix, &[&opener])
        .expect("close_round should succeed");
    assert!(env.account(&env.vault()).is_none());
    assert!(env.account(&round).is_none());
    assert_eq!(env.lamports(&opener.pubkey()), opener_start);
}

#[test]
fn s2_down_wins_when_the_price_ends_below_the_entry_price() {
    let mut env = Env::new();
    let round = env.open_default();
    let (alice, carol) = (env.alice.insecure_clone(), env.carol.insecure_clone());
    let alice_start = env.lamports(&alice.pubkey());
    let carol_start = env.lamports(&carol.pubkey());
    env.join(&round, &alice, Side::Up, 3 * SOL);
    env.join(&round, &carol, Side::Down, 2 * SOL);

    env.settle_at(ENTRY_PRICE - 1);
    assert_eq!(env.round_state(&round).outcome, Outcome::Down);

    // The loser can claim first; it changes nothing for the winner.
    env.claim(&round, &alice.pubkey());
    env.claim(&round, &carol.pubkey());
    assert_eq!(env.lamports(&alice.pubkey()), alice_start - 3 * SOL);
    assert_eq!(env.lamports(&carol.pubkey()), carol_start + 3 * SOL);
    assert_eq!(env.pool(&round), 0);
}

#[test]
fn s2_a_pool_that_does_not_divide_evenly_still_leaves_the_vault_empty() {
    let mut env = Env::new();
    let round = env.open_default();
    let (alice, bob, carol) = (
        env.alice.insecure_clone(),
        env.bob.insecure_clone(),
        env.carol.insecure_clone(),
    );
    let start: Vec<u64> = [&alice, &bob, &carol]
        .iter()
        .map(|k| env.lamports(&k.pubkey()))
        .collect();
    // Two equal winners share a pool of 1,000,005 lamports: 500,002.5 each.
    env.join(&round, &alice, Side::Up, 1);
    env.join(&round, &bob, Side::Up, 1);
    env.join(&round, &carol, Side::Down, 1_000_003);

    env.settle_at(ENTRY_PRICE + 1);
    env.claim(&round, &alice.pubkey());
    env.claim(&round, &bob.pubkey());
    env.claim(&round, &carol.pubkey());

    // The first claim is rounded down and the last winner takes what is left: one lamport
    // more. Nobody gets less than their share rounded down, and nothing stays behind.
    assert_eq!(env.lamports(&alice.pubkey()), start[0] - 1 + 500_002);
    assert_eq!(env.lamports(&bob.pubkey()), start[1] - 1 + 500_003);
    assert_eq!(env.lamports(&carol.pubkey()), start[2] - 1_000_003);
    assert_eq!(env.pool(&round), 0);
}

#[test]
fn s2_settle_can_be_called_by_anyone_and_late_inside_the_window() {
    let mut env = Env::new();
    let round = env.open_default();
    let alice = env.alice.insecure_clone();
    env.join(&round, &alice, Side::Up, SOL);

    // Locking and settling can both be late: here on the last second of the settle window,
    // each with the update that covered its moment.
    env.set_time(EXPIRY + SETTLE_WINDOW_SECS);
    let entry = env.post(entry_update(ENTRY_PRICE));
    let ix = env.lock_ix(&round, entry);
    env.send(ix, &[])
        .expect("lock needs no signer but the fee payer");
    let exit = env.post(exit_update(ENTRY_PRICE + 5));
    let ix = env.settle_ix(&round, exit);
    env.send(ix, &[])
        .expect("settle needs no signer but the fee payer");
    assert_eq!(env.round_state(&round).outcome, Outcome::Up);
}

// ---- refunds: a round that cannot be decided fairly ---------------------------------------

/// Three players in, every stake back, vault empty.
fn assert_everyone_is_refunded(env: &mut Env, round: &common::Pubkey, start: &[u64]) {
    assert_eq!(env.round_state(round).outcome, Outcome::Refund);
    let players = [env.alice.pubkey(), env.bob.pubkey(), env.carol.pubkey()];
    for player in &players {
        env.claim(round, player);
    }
    for (player, start) in players.iter().zip(start) {
        assert_eq!(env.lamports(player), *start);
    }
    assert_eq!(env.pool(round), 0);
}

fn three_players_in(env: &mut Env, sides: [Side; 3]) -> (common::Pubkey, Vec<u64>) {
    let round = env.open_default();
    let players = [
        env.alice.insecure_clone(),
        env.bob.insecure_clone(),
        env.carol.insecure_clone(),
    ];
    let start = players.iter().map(|k| env.lamports(&k.pubkey())).collect();
    for (player, (side, amount)) in players.iter().zip(sides.into_iter().zip([3, 1, 2])) {
        env.join(&round, player, side, amount * SOL);
    }
    (round, start)
}

#[test]
fn a_price_that_ends_exactly_at_the_entry_price_refunds_every_stake() {
    let mut env = Env::new();
    let (round, start) = three_players_in(&mut env, [Side::Up, Side::Up, Side::Down]);
    env.settle_at(ENTRY_PRICE);
    assert_everyone_is_refunded(&mut env, &round, &start);
}

#[test]
fn a_winning_side_with_no_stake_refunds_every_stake() {
    let mut env = Env::new();
    // Everyone chose up and the price fell: there is no one on the down side to pay.
    let (round, start) = three_players_in(&mut env, [Side::Up, Side::Up, Side::Up]);
    env.settle_at(ENTRY_PRICE - 1_000_000);
    assert_everyone_is_refunded(&mut env, &round, &start);
}

#[test]
fn a_round_nobody_settled_inside_the_window_can_be_voided_and_refunds_every_stake() {
    let mut env = Env::new();
    let (round, start) = three_players_in(&mut env, [Side::Up, Side::Down, Side::Down]);
    let before = env.snapshot(&[round, env.vault()]);

    // Not while the settle window is open, even on its last second.
    env.set_time(EXPIRY + SETTLE_WINDOW_SECS);
    let ix = env.void_ix(&round);
    assert_custom_error(env.send(ix, &[]), code(VaultError::SettleWindowOpen));
    assert_eq!(env.snapshot(&[round, env.vault()]), before);

    env.set_time(EXPIRY + SETTLE_WINDOW_SECS + 1);
    // Once the window has closed, settle is refused even with the right update...
    let exit = env.post(exit_update(ENTRY_PRICE + 1));
    let ix = env.settle_ix(&round, exit);
    assert_custom_error(env.send(ix, &[]), code(VaultError::SettleWindowClosed));
    assert_eq!(env.snapshot(&[round, env.vault()]), before);

    let entry = env.post(entry_update(ENTRY_PRICE));
    let ix = env.lock_ix(&round, entry);
    assert_custom_error(env.send(ix, &[]), code(VaultError::SettleWindowClosed));
    assert_eq!(env.snapshot(&[round, env.vault()]), before);

    // ...and anyone can void the round. M3-004 S4: this one was never even locked.
    let ix = env.void_ix(&round);
    env.send(ix, &[]).expect("void_round should succeed");
    assert_everyone_is_refunded(&mut env, &round, &start);

    // A voided round cannot be voided or settled again.
    let ix = env.void_ix(&round);
    assert_custom_error(env.send(ix, &[]), code(VaultError::RoundNotOpen));
}

// ---- S3: prices and timings that must be refused ------------------------------------------

/// A round with two positions, the clock at `now`, and a lock with `entry` that must be
/// refused without changing the round, the vault or either position (M3-004 S3).
fn assert_lock_refused(
    now: i64,
    entry: pyth_solana_receiver_sdk::price_update::PriceUpdateV2,
    owner: Option<common::Pubkey>,
    expected: u32,
) {
    let mut env = Env::new();
    let round = env.open_default();
    let (alice, carol) = (env.alice.insecure_clone(), env.carol.insecure_clone());
    env.join(&round, &alice, Side::Up, 3 * SOL);
    env.join(&round, &carol, Side::Down, 2 * SOL);
    env.set_time(now);
    let entry = env.post_owned_by(entry, owner.unwrap_or(pyth_solana_receiver_sdk::ID));

    let watched = [
        round,
        env.vault(),
        position_pda(&round, &alice.pubkey()),
        position_pda(&round, &carol.pubkey()),
    ];
    let before = env.snapshot(&watched);
    let ix = env.lock_ix(&round, entry);
    assert_custom_error(env.send(ix, &[]), expected);
    assert_eq!(env.snapshot(&watched), before);
    assert!(!env.round_state(&round).locked);

    // A round with no entry price cannot be settled, even at the expiry with the right update.
    env.set_time(EXPIRY);
    let exit = env.post(exit_update(ENTRY_PRICE + 1));
    let ix = env.settle_ix(&round, exit);
    assert_custom_error(env.send(ix, &[]), code(VaultError::RoundNotLocked));
    assert_eq!(env.snapshot(&watched), before);
}

#[test]
fn s3_lock_before_the_join_deadline_is_refused() {
    // The update itself would be valid at the deadline; the clock is one second short, and
    // players can still join.
    assert_lock_refused(
        JOIN_DEADLINE - 1,
        entry_update(ENTRY_PRICE),
        None,
        code(VaultError::LockTooEarly),
    );
}

#[test]
fn s3_lock_rejects_a_price_published_before_the_join_deadline() {
    // A price older than the deadline is one the last players to join could have seen.
    assert_lock_refused(
        JOIN_DEADLINE,
        update(ENTRY_PRICE, JOIN_DEADLINE - 1, JOIN_DEADLINE - 2),
        None,
        code(VaultError::PriceTooEarly),
    );
}

#[test]
fn s3_lock_rejects_a_later_update_than_the_first_one_after_the_join_deadline() {
    assert_lock_refused(
        JOIN_DEADLINE + 10,
        update(ENTRY_PRICE, JOIN_DEADLINE + 2, JOIN_DEADLINE),
        None,
        code(VaultError::PriceNotFirst),
    );
}

#[test]
fn s3_lock_rejects_a_first_update_that_came_too_long_after_the_join_deadline() {
    let late = JOIN_DEADLINE + MAX_PRICE_AGE_SECS + 1;
    assert_lock_refused(
        late,
        update(ENTRY_PRICE, late, JOIN_DEADLINE - 1),
        None,
        code(VaultError::PriceTooLate),
    );
}

#[test]
fn s3_lock_rejects_a_confidence_interval_wider_than_the_limit() {
    let mut wide = entry_update(ENTRY_PRICE);
    wide.price_message.conf = ENTRY_PRICE as u64 * MAX_CONF_BPS / 10_000 + 1;
    assert_lock_refused(
        JOIN_DEADLINE,
        wide,
        None,
        code(VaultError::ConfidenceTooWide),
    );
}

#[test]
fn s3_lock_rejects_an_update_for_another_feed() {
    // A genuine update, for a feed that is not the round's.
    let mut other = entry_update(ENTRY_PRICE);
    other.price_message.feed_id = OTHER_FEED;
    assert_lock_refused(JOIN_DEADLINE, other, None, code(VaultError::WrongFeed));
}

#[test]
fn s3_lock_rejects_a_price_account_the_pyth_receiver_does_not_own() {
    // The right bytes in an account anyone could have written.
    assert_lock_refused(
        JOIN_DEADLINE,
        entry_update(ENTRY_PRICE),
        Some(common::Pubkey::new_unique()),
        ANCHOR_ACCOUNT_OWNED_BY_WRONG_PROGRAM,
    );
}

#[test]
fn s3_lock_rejects_a_partially_verified_update_and_a_non_positive_price() {
    let mut partial = entry_update(ENTRY_PRICE);
    partial.verification_level = VerificationLevel::Partial { num_signatures: 3 };
    assert_lock_refused(
        JOIN_DEADLINE,
        partial,
        None,
        code(VaultError::PriceNotFullyVerified),
    );
    for price in [0, -ENTRY_PRICE] {
        assert_lock_refused(
            JOIN_DEADLINE,
            entry_update(price),
            None,
            code(VaultError::NonPositivePrice),
        );
    }
}

#[test]
fn lock_accepts_a_price_exactly_at_the_latest_time_and_the_confidence_limit() {
    let mut env = Env::new();
    let round = env.open_default();
    let at = JOIN_DEADLINE + MAX_PRICE_AGE_SECS;
    env.set_time(at);
    let mut entry = update(ENTRY_PRICE, at, JOIN_DEADLINE - 1);
    entry.price_message.conf = ENTRY_PRICE as u64 * MAX_CONF_BPS / 10_000;
    let entry = env.post(entry);
    let ix = env.lock_ix(&round, entry);
    env.send(ix, &[])
        .expect("a price on both limits is accepted");
    assert!(env.round_state(&round).locked);
}

#[test]
fn s3_a_locked_round_cannot_be_locked_again_with_another_price() {
    let mut env = Env::new();
    let round = env.open_default();
    env.lock_at(ENTRY_PRICE);
    let before = env.snapshot(&[round, env.vault()]);

    // A second update that would also cover the deadline, with a different price.
    let other = env.post(entry_update(ENTRY_PRICE + 500));
    let ix = env.lock_ix(&round, other);
    assert_custom_error(env.send(ix, &[]), code(VaultError::AlreadyLocked));
    assert_eq!(env.snapshot(&[round, env.vault()]), before);
    assert_eq!(env.round_state(&round).entry_price, ENTRY_PRICE);
}

#[test]
fn a_locked_round_nobody_settled_can_still_be_voided() {
    let mut env = Env::new();
    let (round, start) = three_players_in(&mut env, [Side::Up, Side::Down, Side::Down]);
    env.lock_at(ENTRY_PRICE);
    env.set_time(EXPIRY + SETTLE_WINDOW_SECS + 1);
    let ix = env.void_ix(&round);
    env.send(ix, &[]).expect("void_round should succeed");
    assert_everyone_is_refunded(&mut env, &round, &start);
}

/// A round with two positions, the clock at `now`, and a settle with `exit` that must be
/// refused without changing the round, the vault or either position.
fn assert_settle_refused(
    now: i64,
    exit: pyth_solana_receiver_sdk::price_update::PriceUpdateV2,
    owner: Option<common::Pubkey>,
    expected: u32,
) {
    let mut env = Env::new();
    let round = env.open_default();
    let (alice, carol) = (env.alice.insecure_clone(), env.carol.insecure_clone());
    env.join(&round, &alice, Side::Up, 3 * SOL);
    env.join(&round, &carol, Side::Down, 2 * SOL);
    env.lock_at(ENTRY_PRICE);
    env.set_time(now);
    let exit = env.post_owned_by(exit, owner.unwrap_or(pyth_solana_receiver_sdk::ID));

    let watched = [
        round,
        env.vault(),
        position_pda(&round, &alice.pubkey()),
        position_pda(&round, &carol.pubkey()),
        alice.pubkey(),
        carol.pubkey(),
    ];
    let before = env.snapshot(&watched);
    let ix = env.settle_ix(&round, exit);
    assert_custom_error(env.send(ix, &[]), expected);
    assert_eq!(env.snapshot(&watched), before);
    assert_eq!(env.round_state(&round).outcome, Outcome::Open);
    assert_eq!(env.pool(&round), 5 * SOL);

    // Nothing can be claimed from a round that did not settle.
    let ix = env.claim_ix(&round, &alice.pubkey());
    assert_custom_error(env.send(ix, &[]), code(VaultError::RoundNotSettled));
    assert_eq!(env.snapshot(&watched), before);
}

#[test]
fn s3_settle_before_expiry_is_refused() {
    // The update itself would be valid at the expiry; the clock is one second short.
    assert_settle_refused(
        EXPIRY - 1,
        exit_update(ENTRY_PRICE + 1),
        None,
        code(VaultError::SettleTooEarly),
    );
}

#[test]
fn s3_settle_rejects_a_price_published_before_the_expiry() {
    // The freshest price there was one second before the round ended is too old to end it.
    assert_settle_refused(
        EXPIRY,
        update(ENTRY_PRICE + 1, EXPIRY - 1, EXPIRY - 2),
        None,
        code(VaultError::PriceTooEarly),
    );
}

#[test]
fn s3_settle_rejects_a_later_update_than_the_first_one_after_the_expiry() {
    // Published after the expiry, but the update before it was already at or after the
    // expiry, so this is not the one that covers it. A caller cannot shop for a price.
    assert_settle_refused(
        EXPIRY + 10,
        update(ENTRY_PRICE + 1, EXPIRY + 2, EXPIRY),
        None,
        code(VaultError::PriceNotFirst),
    );
}

#[test]
fn s3_settle_rejects_a_first_update_that_came_too_long_after_the_expiry() {
    // The feed was silent across the expiry and for longer than the maximum age after it.
    let late = EXPIRY + MAX_PRICE_AGE_SECS + 1;
    assert_settle_refused(
        late,
        update(ENTRY_PRICE + 1, late, EXPIRY - 1),
        None,
        code(VaultError::PriceTooLate),
    );
}

#[test]
fn s3_settle_rejects_a_confidence_interval_wider_than_the_limit() {
    let mut wide = exit_update(ENTRY_PRICE + 1);
    wide.price_message.conf = (ENTRY_PRICE as u64 + 1) * MAX_CONF_BPS / 10_000 + 1;
    assert_settle_refused(EXPIRY, wide, None, code(VaultError::ConfidenceTooWide));
}

#[test]
fn s3_settle_rejects_an_update_for_another_feed() {
    let mut other = exit_update(ENTRY_PRICE + 1);
    other.price_message.feed_id = OTHER_FEED;
    assert_settle_refused(EXPIRY, other, None, code(VaultError::WrongFeed));
}

#[test]
fn s3_settle_rejects_a_price_account_the_pyth_receiver_does_not_own() {
    assert_settle_refused(
        EXPIRY,
        exit_update(ENTRY_PRICE + 1),
        Some(common::Pubkey::new_unique()),
        ANCHOR_ACCOUNT_OWNED_BY_WRONG_PROGRAM,
    );
}

#[test]
fn s3_settle_rejects_a_partially_verified_update_and_a_changed_exponent() {
    let mut partial = exit_update(ENTRY_PRICE + 1);
    partial.verification_level = VerificationLevel::Partial { num_signatures: 5 };
    assert_settle_refused(
        EXPIRY,
        partial,
        None,
        code(VaultError::PriceNotFullyVerified),
    );

    let mut rescaled = exit_update(ENTRY_PRICE + 1);
    rescaled.price_message.exponent = EXPONENT + 1;
    assert_settle_refused(EXPIRY, rescaled, None, code(VaultError::ExponentChanged));
}

#[test]
fn s3_a_settled_round_cannot_be_settled_again_with_another_price() {
    let mut env = Env::new();
    let round = env.open_default();
    let (alice, carol) = (env.alice.insecure_clone(), env.carol.insecure_clone());
    env.join(&round, &alice, Side::Up, 3 * SOL);
    env.join(&round, &carol, Side::Down, 2 * SOL);
    env.settle_at(ENTRY_PRICE + 1);
    let before = env.snapshot(&[round, env.vault()]);

    // An update that would flip the result.
    let flip = env.post(exit_update(ENTRY_PRICE - 1));
    let ix = env.settle_ix(&round, flip);
    assert_custom_error(env.send(ix, &[]), code(VaultError::RoundNotOpen));
    assert_eq!(env.snapshot(&[round, env.vault()]), before);
    assert_eq!(env.round_state(&round).outcome, Outcome::Up);

    // Nor can its entry price be set again.
    let entry = env.post(entry_update(ENTRY_PRICE - 5));
    let ix = env.lock_ix(&round, entry);
    assert_custom_error(env.send(ix, &[]), code(VaultError::RoundNotOpen));
    assert_eq!(env.snapshot(&[round, env.vault()]), before);
}
