use {
    crate::{
        error::VaultError,
        state::{Outcome, Round, SETTLE_WINDOW_SECS},
    },
    anchor_lang::prelude::*,
};

#[derive(Accounts)]
pub struct VoidRound<'info> {
    #[account(mut)]
    pub round: Account<'info, Round>,
}

pub(crate) fn handler(ctx: Context<VoidRound>) -> Result<()> {
    let round = &mut ctx.accounts.round;
    require!(round.outcome == Outcome::Open, VaultError::RoundNotOpen);
    let window_end = round
        .expiry
        .checked_add(SETTLE_WINDOW_SECS)
        .ok_or(VaultError::ArithmeticOverflow)?;
    require!(
        Clock::get()?.unix_timestamp > window_end,
        VaultError::SettleWindowOpen
    );

    round.outcome = Outcome::Refund;
    round.remaining_pool = round
        .total_up
        .checked_add(round.total_down)
        .ok_or(VaultError::ArithmeticOverflow)?;
    Ok(())
}
