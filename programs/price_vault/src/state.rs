use anchor_lang::prelude::*;

pub const ROUND_SEED: &[u8] = b"round";
pub const VAULT_SEED: &[u8] = b"vault";
pub const POSITION_SEED: &[u8] = b"position";

/// The entry price must have been published no more than this long after the join deadline,
/// and the exit price no more than this long after the expiry.
pub const MAX_PRICE_AGE_SECS: i64 = 60;

/// Shortest time between the join deadline, when the entry price is taken, and the expiry,
/// when the exit price is.
pub const MIN_LOCK_SECS: i64 = 60;

/// Widest confidence interval accepted, in basis points of the price (50 = 0.5%).
pub const MAX_CONF_BPS: u64 = 50;

/// How long after expiry a round can be settled. After that it can only be voided, which
/// refunds every stake, so a missing price cannot lock funds.
pub const SETTLE_WINDOW_SECS: i64 = 3_600;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum Side {
    Up,
    Down,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum Outcome {
    /// Not settled yet.
    Open,
    /// The exit price is above the entry price: the up side shares the pool.
    Up,
    /// The exit price is below the entry price: the down side shares the pool.
    Down,
    /// Every position gets its own stake back.
    Refund,
}

#[account]
#[derive(InitSpace)]
pub struct Round {
    pub opener: Pubkey,
    pub round_id: u64,
    /// Pyth price feed id the round opens and settles on.
    pub feed_id: [u8; 32],
    /// Whether the entry price has been fixed. It is fixed after the join deadline, from the
    /// update that covers the deadline, so no player knew it when they chose a side.
    pub locked: bool,
    /// Entry price, as `entry_price * 10^exponent`; zero until locked.
    pub entry_price: i64,
    pub entry_conf: u64,
    pub exponent: i32,
    pub entry_publish_time: i64,
    /// Positions are accepted while the clock is before this. The round's entry price is
    /// the first price published at or after it.
    pub join_deadline: i64,
    /// The round settles on the first price published at or after this.
    pub expiry: i64,
    pub total_up: u64,
    pub total_down: u64,
    pub outcome: Outcome,
    /// Exit price and its publish time; zero until settled.
    pub exit_price: i64,
    pub exit_conf: u64,
    pub exit_publish_time: i64,
    /// Lamports of stake still in the vault and owed to unclaimed positions.
    pub remaining_pool: u64,
    /// Winning-side stake that has not claimed yet; the divisor for the next payout.
    pub remaining_winning_stake: u64,
    /// Positions not yet claimed. The round can close when this is zero.
    pub open_positions: u32,
}

/// Holds a round's stakes as lamports above its own rent. It carries no other data, so the
/// pool is exactly `lamports - rent`.
#[account]
#[derive(InitSpace)]
pub struct Vault {
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Position {
    pub round: Pubkey,
    pub player: Pubkey,
    pub side: Side,
    pub amount: u64,
    pub bump: u8,
}
