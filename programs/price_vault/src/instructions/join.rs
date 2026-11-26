use {
    crate::{
        error::VaultError,
        state::{Position, Round, Side, Vault, POSITION_SEED, VAULT_SEED},
    },
    anchor_lang::{prelude::*, system_program},
};

#[derive(Accounts)]
pub struct Join<'info> {
    #[account(mut)]
    pub player: Signer<'info>,
    #[account(mut)]
    pub round: Account<'info, Round>,
    #[account(mut, seeds = [VAULT_SEED, round.key().as_ref()], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    #[account(
        init,
        payer = player,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, round.key().as_ref(), player.key().as_ref()],
        bump
    )]
    pub position: Account<'info, Position>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn handler(ctx: Context<Join>, side: Side, amount: u64) -> Result<()> {
    require!(amount > 0, VaultError::ZeroStake);
    let round = &mut ctx.accounts.round;
    // A round cannot be settled or voided before its expiry, and the join deadline is never
    // after the expiry, so this one check also keeps positions out of a finished round.
    require!(
        Clock::get()?.unix_timestamp < round.join_deadline,
        VaultError::JoinClosed
    );

    let side_total = match side {
        Side::Up => &mut round.total_up,
        Side::Down => &mut round.total_down,
    };
    *side_total = side_total
        .checked_add(amount)
        .ok_or(VaultError::ArithmeticOverflow)?;
    // The pool is the sum of both sides; settle relies on it fitting in a u64.
    round
        .total_up
        .checked_add(round.total_down)
        .ok_or(VaultError::ArithmeticOverflow)?;
    round.open_positions = round
        .open_positions
        .checked_add(1)
        .ok_or(VaultError::ArithmeticOverflow)?;

    ctx.accounts.position.set_inner(Position {
        round: round.key(),
        player: ctx.accounts.player.key(),
        side,
        amount,
        bump: ctx.bumps.position,
    });

    system_program::transfer(
        CpiContext::new(
            ctx.accounts.system_program.key(),
            system_program::Transfer {
                from: ctx.accounts.player.to_account_info(),
                to: ctx.accounts.vault.to_account_info(),
            },
        ),
        amount,
    )
}
