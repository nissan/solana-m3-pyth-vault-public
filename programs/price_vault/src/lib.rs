//! Price-settled vault: players stake SOL on whether a Pyth price ends above or below
//! where it started, and the round settles from the one price update that covers its expiry.
//!
//! Flow: `open_round` fixes the feed and the schedule; `join` moves a stake into the round's
//! vault; `lock_round` fixes the entry price from the update that covers the join deadline,
//! after the last player has chosen a side; `settle` fixes the result from the price at
//! expiry; `claim` pays one position; `close_round` returns the opener's rent. A round
//! nobody can lock or settle fairly is refunded (`void_round`, a tie, or an empty winning
//! side).

use anchor_lang::prelude::*;

pub mod error;
pub mod instructions;
pub mod oracle;
pub mod state;

use {instructions::*, state::Side};

declare_id!("Br6Fg6ZdQLTKxyr9D82fEfh1a1Rz3DN1bLaGZVuQ38H7");

#[program]
pub mod price_vault {
    use super::*;

    /// Open a round on `feed_id`. It has no entry price until `lock_round`.
    pub fn open_round(
        ctx: Context<OpenRound>,
        round_id: u64,
        feed_id: [u8; 32],
        join_deadline: i64,
        expiry: i64,
    ) -> Result<()> {
        instructions::open_round::handler(ctx, round_id, feed_id, join_deadline, expiry)
    }

    /// Stake `amount` lamports on one side. One position per player per round.
    pub fn join(ctx: Context<Join>, side: Side, amount: u64) -> Result<()> {
        instructions::join::handler(ctx, side, amount)
    }

    /// Fix the entry price from the price update that covers the join deadline. Anyone may
    /// call it: the update decides, not the caller.
    pub fn lock_round(ctx: Context<LockRound>) -> Result<()> {
        instructions::lock_round::handler(ctx)
    }

    /// Fix the result from the price update that covers the round's expiry. Anyone may call
    /// it: the update decides, not the caller.
    pub fn settle(ctx: Context<Settle>) -> Result<()> {
        instructions::settle::handler(ctx)
    }

    /// Turn a round nobody locked or settled inside the settle window into a refund. Anyone may call it.
    pub fn void_round(ctx: Context<VoidRound>) -> Result<()> {
        instructions::void_round::handler(ctx)
    }

    /// Pay one position what the result gives it and close it. Anyone may call it: the
    /// payout can only go to the position's own player.
    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        instructions::claim::handler(ctx)
    }

    /// Close a finished round and its empty vault, returning their rent to the opener.
    pub fn close_round(ctx: Context<CloseRound>) -> Result<()> {
        instructions::close_round::handler(ctx)
    }
}
