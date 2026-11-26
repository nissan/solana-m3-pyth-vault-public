use {
    crate::{
        error::VaultError,
        state::{Outcome, Round, Vault, MIN_LOCK_SECS, ROUND_SEED, SETTLE_WINDOW_SECS, VAULT_SEED},
    },
    anchor_lang::prelude::*,
};

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct OpenRound<'info> {
    #[account(mut)]
    pub opener: Signer<'info>,
    #[account(
        init,
        payer = opener,
        space = 8 + Round::INIT_SPACE,
        seeds = [ROUND_SEED, opener.key().as_ref(), &round_id.to_le_bytes()],
        bump
    )]
    pub round: Account<'info, Round>,
    #[account(
        init,
        payer = opener,
        space = 8 + Vault::INIT_SPACE,
        seeds = [VAULT_SEED, round.key().as_ref()],
        bump
    )]
    pub vault: Account<'info, Vault>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn handler(
    ctx: Context<OpenRound>,
    round_id: u64,
    feed_id: [u8; 32],
    join_deadline: i64,
    expiry: i64,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    // Saturating: an expiry too small to subtract from is before `now` and fails below.
    let latest_deadline = expiry.saturating_sub(MIN_LOCK_SECS);
    require!(
        now < join_deadline && join_deadline <= latest_deadline,
        VaultError::InvalidSchedule
    );
    // lock, settle and void add the window to the expiry; refuse a round where that overflows.
    expiry
        .checked_add(SETTLE_WINDOW_SECS)
        .ok_or(VaultError::ArithmeticOverflow)?;

    // No price is read here. The entry price is fixed by `lock_round` after the join
    // deadline, so nobody choosing a side can compare the market with it.
    ctx.accounts.round.set_inner(Round {
        opener: ctx.accounts.opener.key(),
        round_id,
        feed_id,
        locked: false,
        entry_price: 0,
        entry_conf: 0,
        exponent: 0,
        entry_publish_time: 0,
        join_deadline,
        expiry,
        total_up: 0,
        total_down: 0,
        outcome: Outcome::Open,
        exit_price: 0,
        exit_conf: 0,
        exit_publish_time: 0,
        remaining_pool: 0,
        remaining_winning_stake: 0,
        open_positions: 0,
    });
    ctx.accounts.vault.bump = ctx.bumps.vault;
    Ok(())
}
