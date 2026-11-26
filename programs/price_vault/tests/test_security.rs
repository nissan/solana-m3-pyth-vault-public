//! One failing test per security check in docs/SECURITY.md: signer, account owner, PDA
//! seeds and arithmetic overflow, for each instruction they apply to. Every rejection is
//! checked for its named error and for leaving the accounts as they were.

mod common;

use {
    anchor_lang::AccountSerialize,
    common::*,
    price_vault::{
        error::VaultError,
        state::{Outcome, Position, Side},
    },
    solana_keypair::Keypair,
    solana_signer::Signer,
};

/// A second round, opened by Bob, to borrow accounts from.
fn open_second_round(env: &mut Env) -> Pubkey {
    let bob = env.bob.insecure_clone();
    let ix = env.open_ix(&bob.pubkey(), 99, FEED, JOIN_DEADLINE, EXPIRY);
    env.send(ix, &[&bob]).expect("second round should open");
    round_pda(&bob.pubkey(), 99)
}

// ---- signer -------------------------------------------------------------------------------

#[test]
fn open_needs_the_openers_signature() {
    let mut env = Env::new();
    let opener = env.opener.pubkey();
    let watched = [opener, env.round(), env.vault()];
    let before = env.snapshot(&watched);

    let ix = without_signer(env.open_default_ix(), &opener);
    assert_custom_error(env.send(ix, &[]), ANCHOR_ACCOUNT_NOT_SIGNER);
    assert_eq!(env.snapshot(&watched), before);
}

#[test]
fn join_needs_the_players_signature() {
    let mut env = Env::new();
    let round = env.open_default();
    let alice = env.alice.pubkey();
    let watched = [alice, round, env.vault(), position_pda(&round, &alice)];
    let before = env.snapshot(&watched);

    // Nobody can stake another player's SOL for them.
    let ix = without_signer(env.join_ix(&round, &alice, Side::Up, SOL), &alice);
    assert_custom_error(env.send(ix, &[]), ANCHOR_ACCOUNT_NOT_SIGNER);
    assert_eq!(env.snapshot(&watched), before);
}

#[test]
fn close_round_needs_the_openers_signature_and_the_right_opener() {
    let mut env = Env::new();
    let round = env.open_default();
    env.settle_at(ENTRY_PRICE + 1);
    let opener = env.opener.pubkey();
    let mallory = Keypair::new();
    let watched = [opener, mallory.pubkey(), round, env.vault()];
    let before = env.snapshot(&watched);

    let ix = without_signer(env.close_ix(&round, &opener), &opener);
    assert_custom_error(env.send(ix, &[]), ANCHOR_ACCOUNT_NOT_SIGNER);

    // Someone else signing cannot collect the opener's rent.
    let ix = env.close_ix(&round, &mallory.pubkey());
    assert_custom_error(env.send(ix, &[&mallory]), ANCHOR_CONSTRAINT_HAS_ONE);
    assert_eq!(env.snapshot(&watched), before);
}

// ---- claim: only to the position's own player, only once, only after settlement -----------

#[test]
fn claim_cannot_pay_anyone_but_the_positions_player() {
    let mut env = Env::new();
    let round = env.open_default();
    let (alice, carol) = (env.alice.insecure_clone(), env.carol.insecure_clone());
    env.join(&round, &alice, Side::Up, 3 * SOL);
    env.join(&round, &carol, Side::Down, 2 * SOL);
    env.settle_at(ENTRY_PRICE + 1);
    let mallory = Keypair::new();
    let watched = [
        round,
        env.vault(),
        position_pda(&round, &alice.pubkey()),
        alice.pubkey(),
        mallory.pubkey(),
    ];
    let before = env.snapshot(&watched);

    // Alice's winning position, with Mallory named as the player to pay.
    let mut accounts = env.claim_accounts(&round, &alice.pubkey());
    accounts.player = mallory.pubkey();
    let ix = env.claim_ix_with(accounts);
    assert_custom_error(env.send(ix, &[]), ANCHOR_CONSTRAINT_SEEDS);
    assert_eq!(env.snapshot(&watched), before);
}

#[test]
fn claim_pays_a_position_once() {
    let mut env = Env::new();
    let round = env.open_default();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.join(&round, &alice, Side::Up, 3 * SOL);
    env.join(&round, &bob, Side::Up, SOL);
    env.settle_at(ENTRY_PRICE + 1);
    env.claim(&round, &alice.pubkey());
    let watched = [round, env.vault(), alice.pubkey(), bob.pubkey()];
    let before = env.snapshot(&watched);

    // The position closed with the first claim; Bob's share is still in the vault.
    let ix = env.claim_ix(&round, &alice.pubkey());
    assert_custom_error(env.send(ix, &[]), ANCHOR_ACCOUNT_NOT_INITIALIZED);
    assert_eq!(env.snapshot(&watched), before);
    assert_eq!(env.pool(&round), SOL);
}

#[test]
fn claim_before_settlement_is_refused() {
    let mut env = Env::new();
    let round = env.open_default();
    let alice = env.alice.insecure_clone();
    env.join(&round, &alice, Side::Up, SOL);
    let watched = [
        round,
        env.vault(),
        position_pda(&round, &alice.pubkey()),
        alice.pubkey(),
    ];
    let before = env.snapshot(&watched);

    // No early exit: not before the expiry, and not after it until the round is settled.
    for now in [T0, EXPIRY + 1] {
        env.set_time(now);
        let ix = env.claim_ix(&round, &alice.pubkey());
        assert_custom_error(env.send(ix, &[]), code(VaultError::RoundNotSettled));
        assert_eq!(env.snapshot(&watched), before);
    }
}

// ---- PDA seeds and account owner ----------------------------------------------------------

#[test]
fn join_rejects_another_rounds_vault() {
    let mut env = Env::new();
    let round = env.open_default();
    let other_round = open_second_round(&mut env);
    let alice = env.alice.insecure_clone();
    let watched = [
        alice.pubkey(),
        round,
        env.vault(),
        vault_pda(&other_round),
        position_pda(&round, &alice.pubkey()),
    ];
    let before = env.snapshot(&watched);

    let mut accounts = env.join_accounts(&round, &alice.pubkey());
    accounts.vault = vault_pda(&other_round);
    let ix = env.join_ix_with(accounts, Side::Up, SOL);
    assert_custom_error(env.send(ix, &[&alice]), ANCHOR_CONSTRAINT_SEEDS);
    assert_eq!(env.snapshot(&watched), before);
}

#[test]
fn claim_rejects_another_rounds_vault_and_another_rounds_position() {
    let mut env = Env::new();
    let round = env.open_default();
    let other_round = open_second_round(&mut env);
    let (alice, carol) = (env.alice.insecure_clone(), env.carol.insecure_clone());
    env.join(&round, &alice, Side::Up, SOL);
    env.join(&round, &carol, Side::Down, SOL);
    // The other round holds far more, and Alice has a position there too.
    env.join(&other_round, &alice, Side::Up, 50 * SOL);
    env.settle_at(ENTRY_PRICE + 1);
    let watched = [
        round,
        other_round,
        env.vault(),
        vault_pda(&other_round),
        position_pda(&round, &alice.pubkey()),
        position_pda(&other_round, &alice.pubkey()),
        alice.pubkey(),
    ];
    let before = env.snapshot(&watched);

    // Paying this round's result out of the other round's vault.
    let mut accounts = env.claim_accounts(&round, &alice.pubkey());
    accounts.vault = vault_pda(&other_round);
    let ix = env.claim_ix_with(accounts);
    assert_custom_error(env.send(ix, &[]), ANCHOR_CONSTRAINT_SEEDS);

    // Claiming the large position from the unsettled round against this settled round.
    let mut accounts = env.claim_accounts(&round, &alice.pubkey());
    accounts.position = position_pda(&other_round, &alice.pubkey());
    let ix = env.claim_ix_with(accounts);
    assert_custom_error(env.send(ix, &[]), ANCHOR_CONSTRAINT_SEEDS);
    assert_eq!(env.snapshot(&watched), before);
}

#[test]
fn claim_rejects_a_position_account_the_program_does_not_own() {
    let mut env = Env::new();
    let round = env.open_default();
    let (alice, carol) = (env.alice.insecure_clone(), env.carol.insecure_clone());
    env.join(&round, &alice, Side::Up, SOL);
    env.join(&round, &carol, Side::Down, SOL);
    env.settle_at(ENTRY_PRICE + 1);

    // A forged position: the right layout, claiming a winning stake of the whole pool,
    // in an account another program owns.
    let mallory = Keypair::new();
    let forged = Pubkey::new_unique();
    let mut data = Vec::new();
    Position {
        round,
        player: mallory.pubkey(),
        side: Side::Up,
        amount: SOL,
        bump: 255,
    }
    .try_serialize(&mut data)
    .unwrap();
    env.put(forged, Pubkey::new_unique(), data);
    let watched = [round, env.vault(), mallory.pubkey()];
    let before = env.snapshot(&watched);

    let mut accounts = env.claim_accounts(&round, &mallory.pubkey());
    accounts.position = forged;
    let ix = env.claim_ix_with(accounts);
    assert_custom_error(env.send(ix, &[]), ANCHOR_ACCOUNT_OWNED_BY_WRONG_PROGRAM);
    assert_eq!(env.snapshot(&watched), before);
}

#[test]
fn settle_rejects_a_round_account_the_program_does_not_own() {
    let mut env = Env::new();
    let round = env.open_default();
    // A copy of the real round's bytes in an account another program owns.
    let forged = Pubkey::new_unique();
    let data = env.account(&round).unwrap().data;
    env.put(forged, Pubkey::new_unique(), data);
    env.set_time(EXPIRY);
    let exit = env.post(exit_update(ENTRY_PRICE + 1));

    let ix = env.settle_ix(&forged, exit);
    assert_custom_error(env.send(ix, &[]), ANCHOR_ACCOUNT_OWNED_BY_WRONG_PROGRAM);
    let entry = env.post(entry_update(ENTRY_PRICE));
    let ix = env.lock_ix(&forged, entry);
    assert_custom_error(env.send(ix, &[]), ANCHOR_ACCOUNT_OWNED_BY_WRONG_PROGRAM);
    assert_eq!(env.round_state(&round).outcome, Outcome::Open);
    assert!(!env.round_state(&round).locked);
}

#[test]
fn close_round_rejects_another_rounds_vault() {
    let mut env = Env::new();
    let round = env.open_default();
    let other_round = open_second_round(&mut env);
    env.settle_at(ENTRY_PRICE + 1);
    let opener = env.opener.insecure_clone();
    let watched = [opener.pubkey(), round, env.vault(), vault_pda(&other_round)];
    let before = env.snapshot(&watched);

    // Closing Bob's vault into the opener's wallet.
    let mut ix = env.close_ix(&round, &opener.pubkey());
    ix.accounts[2].pubkey = vault_pda(&other_round);
    assert_custom_error(env.send(ix, &[&opener]), ANCHOR_CONSTRAINT_SEEDS);
    assert_eq!(env.snapshot(&watched), before);
}

// ---- close_round: not before the round is finished and paid out ---------------------------

#[test]
fn close_round_is_refused_before_settlement_and_while_positions_are_unclaimed() {
    let mut env = Env::new();
    let round = env.open_default();
    let (alice, carol) = (env.alice.insecure_clone(), env.carol.insecure_clone());
    env.join(&round, &alice, Side::Up, 3 * SOL);
    env.join(&round, &carol, Side::Down, 2 * SOL);
    let opener = env.opener.insecure_clone();
    let watched = [opener.pubkey(), round, env.vault()];

    let before = env.snapshot(&watched);
    let ix = env.close_ix(&round, &opener.pubkey());
    assert_custom_error(env.send(ix, &[&opener]), code(VaultError::RoundNotSettled));
    assert_eq!(env.snapshot(&watched), before);

    // Settled, the winner unpaid: the opener cannot close the vault over the pool.
    env.settle_at(ENTRY_PRICE + 1);
    env.claim(&round, &carol.pubkey());
    let before = env.snapshot(&watched);
    let ix = env.close_ix(&round, &opener.pubkey());
    assert_custom_error(
        env.send(ix, &[&opener]),
        code(VaultError::PositionsOutstanding),
    );
    assert_eq!(env.snapshot(&watched), before);
    assert_eq!(env.pool(&round), 5 * SOL);
}

// ---- arithmetic overflow ------------------------------------------------------------------

#[test]
fn join_rejects_a_stake_that_would_overflow_a_side_or_the_pool() {
    let mut env = Env::new();
    let round = env.open_default();
    let alice = env.alice.insecure_clone();

    // (total_up, total_down) written straight into the round; no real stake can reach them.
    for (total_up, total_down) in [(u64::MAX - 5, 0), (0, u64::MAX - 5)] {
        let mut state = env.round_state(&round);
        state.total_up = total_up;
        state.total_down = total_down;
        env.set_round_state(&round, &state);
        let watched = [
            alice.pubkey(),
            round,
            env.vault(),
            position_pda(&round, &alice.pubkey()),
        ];
        let before = env.snapshot(&watched);

        // Up 10 overflows the up side in the first case and the sum of both in the second.
        let ix = env.join_ix(&round, &alice.pubkey(), Side::Up, 10);
        assert_custom_error(
            env.send(ix, &[&alice]),
            code(VaultError::ArithmeticOverflow),
        );
        assert_eq!(env.snapshot(&watched), before);
    }
}

#[test]
fn open_rejects_an_expiry_whose_settle_window_would_overflow() {
    let mut env = Env::new();
    let opener = env.opener.insecure_clone();
    let watched = [opener.pubkey(), env.round(), env.vault()];
    let before = env.snapshot(&watched);

    let ix = env.open_ix(&opener.pubkey(), ROUND_ID, FEED, JOIN_DEADLINE, i64::MAX);
    assert_custom_error(
        env.send(ix, &[&opener]),
        code(VaultError::ArithmeticOverflow),
    );
    assert_eq!(env.snapshot(&watched), before);
}
