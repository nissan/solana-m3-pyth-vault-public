use {
    crate::{
        error::VaultError,
        state::{Outcome, Position, Round, Side, Vault, POSITION_SEED, VAULT_SEED},
    },
    anchor_lang::prelude::*,
};

#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(mut)]
    pub round: Account<'info, Round>,
    #[account(mut, seeds = [VAULT_SEED, round.key().as_ref()], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(
        mut,
        close = player,
        seeds = [POSITION_SEED, round.key().as_ref(), player.key().as_ref()],
        bump = position.bump
    )]
    pub position: Account<'info, Position>,
    /// CHECK: receives lamports only. The position's address is derived from this key and
    /// the round, so only the player who opened the position can be named here.
    #[account(mut)]
    pub player: UncheckedAccount<'info>,
}

pub(crate) fn handler(ctx: Context<Claim>) -> Result<()> {
    let round = &mut ctx.accounts.round;
    let position = &ctx.accounts.position;

    let payout = match (round.outcome, position.side) {
        (Outcome::Open, _) => return err!(VaultError::RoundNotSettled),
        (Outcome::Refund, _) => position.amount,
        (Outcome::Up, Side::Up) | (Outcome::Down, Side::Down) => {
            // This position's share of what is left, over the winning stake that is left.
            // The last winner's stake equals the remaining winning stake, so it takes
            // exactly the remaining pool and no lamport stays behind.
            let share = u128::from(position.amount)
                .checked_mul(u128::from(round.remaining_pool))
                .ok_or(VaultError::ArithmeticOverflow)?
                .checked_div(u128::from(round.remaining_winning_stake))
                .ok_or(VaultError::ArithmeticOverflow)?;
            round.remaining_winning_stake = round
                .remaining_winning_stake
                .checked_sub(position.amount)
                .ok_or(VaultError::ArithmeticOverflow)?;
            u64::try_from(share).map_err(|_| VaultError::ArithmeticOverflow)?
        }
        (Outcome::Up, Side::Down) | (Outcome::Down, Side::Up) => 0,
    };

    round.remaining_pool = round
        .remaining_pool
        .checked_sub(payout)
        .ok_or(VaultError::ArithmeticOverflow)?;
    round.open_positions = round
        .open_positions
        .checked_sub(1)
        .ok_or(VaultError::ArithmeticOverflow)?;

    ctx.accounts.vault.sub_lamports(payout)?;
    ctx.accounts.player.add_lamports(payout)?;
    Ok(())
}
