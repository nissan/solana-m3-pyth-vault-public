use {
    crate::{
        error::VaultError,
        oracle,
        state::{Outcome, Round, SETTLE_WINDOW_SECS},
    },
    anchor_lang::prelude::*,
    pyth_solana_receiver_sdk::price_update::PriceUpdateV2,
    std::cmp::Ordering,
};

#[derive(Accounts)]
pub struct Settle<'info> {
    #[account(mut)]
    pub round: Account<'info, Round>,
    /// Must be owned by the Pyth receiver program; the account type checks that.
    pub price_update: Account<'info, PriceUpdateV2>,
}

pub(crate) fn handler(ctx: Context<Settle>) -> Result<()> {
    let round = &mut ctx.accounts.round;
    require!(round.outcome == Outcome::Open, VaultError::RoundNotOpen);

    let now = Clock::get()?.unix_timestamp;
    require!(now >= round.expiry, VaultError::SettleTooEarly);
    let window_end = round
        .expiry
        .checked_add(SETTLE_WINDOW_SECS)
        .ok_or(VaultError::ArithmeticOverflow)?;
    require!(now <= window_end, VaultError::SettleWindowClosed);

    require!(round.locked, VaultError::RoundNotLocked);
    let exit = oracle::read_covering(&ctx.accounts.price_update, &round.feed_id, round.expiry)?;
    require!(exit.exponent == round.exponent, VaultError::ExponentChanged);

    let (outcome, winning_stake) = match exit.price.cmp(&round.entry_price) {
        Ordering::Greater => (Outcome::Up, round.total_up),
        Ordering::Less => (Outcome::Down, round.total_down),
        Ordering::Equal => (Outcome::Refund, 0),
    };
    // With nobody on the winning side there is no one to pay the pool to: refund instead.
    round.outcome = if winning_stake == 0 {
        Outcome::Refund
    } else {
        outcome
    };
    round.remaining_winning_stake = winning_stake;
    round.remaining_pool = round
        .total_up
        .checked_add(round.total_down)
        .ok_or(VaultError::ArithmeticOverflow)?;
    round.exit_price = exit.price;
    round.exit_conf = exit.conf;
    round.exit_publish_time = exit.publish_time;
    Ok(())
}
