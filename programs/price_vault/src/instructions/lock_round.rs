use {
    crate::{
        error::VaultError,
        oracle,
        state::{Outcome, Round, SETTLE_WINDOW_SECS},
    },
    anchor_lang::prelude::*,
    pyth_solana_receiver_sdk::price_update::PriceUpdateV2,
};

#[derive(Accounts)]
pub struct LockRound<'info> {
    #[account(mut)]
    pub round: Account<'info, Round>,
    /// Must be owned by the Pyth receiver program; the account type checks that.
    pub price_update: Account<'info, PriceUpdateV2>,
}

pub(crate) fn handler(ctx: Context<LockRound>) -> Result<()> {
    let round = &mut ctx.accounts.round;
    require!(round.outcome == Outcome::Open, VaultError::RoundNotOpen);
    require!(!round.locked, VaultError::AlreadyLocked);

    let now = Clock::get()?.unix_timestamp;
    require!(now >= round.join_deadline, VaultError::LockTooEarly);
    let window_end = round
        .expiry
        .checked_add(SETTLE_WINDOW_SECS)
        .ok_or(VaultError::ArithmeticOverflow)?;
    require!(now <= window_end, VaultError::SettleWindowClosed);

    let entry = oracle::read_covering(
        &ctx.accounts.price_update,
        &round.feed_id,
        round.join_deadline,
    )?;
    round.locked = true;
    round.entry_price = entry.price;
    round.entry_conf = entry.conf;
    round.exponent = entry.exponent;
    round.entry_publish_time = entry.publish_time;
    Ok(())
}
