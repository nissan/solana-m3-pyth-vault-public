use {
    crate::{
        error::VaultError,
        state::{Outcome, Round, Vault, VAULT_SEED},
    },
    anchor_lang::prelude::*,
};

#[derive(Accounts)]
pub struct CloseRound<'info> {
    #[account(mut)]
    pub opener: Signer<'info>,
    #[account(mut, close = opener, has_one = opener)]
    pub round: Account<'info, Round>,
    #[account(mut, close = opener, seeds = [VAULT_SEED, round.key().as_ref()], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
}

pub(crate) fn handler(ctx: Context<CloseRound>) -> Result<()> {
    let round = &ctx.accounts.round;
    require!(round.outcome != Outcome::Open, VaultError::RoundNotSettled);
    require!(
        round.open_positions == 0 && round.remaining_pool == 0,
        VaultError::PositionsOutstanding
    );
    Ok(())
}
