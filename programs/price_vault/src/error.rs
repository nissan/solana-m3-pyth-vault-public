use anchor_lang::prelude::*;

#[error_code]
pub enum VaultError {
    #[msg(
        "The join deadline must be in the future and at least the lock period before the expiry"
    )]
    InvalidSchedule,
    #[msg("The price's confidence interval is wider than the limit")]
    ConfidenceTooWide,
    #[msg("The price update is for a different feed than the round's")]
    WrongFeed,
    #[msg("The price update was only partially verified by the Pyth receiver")]
    PriceNotFullyVerified,
    #[msg("The price is zero or negative")]
    NonPositivePrice,
    #[msg("The price's exponent differs from the entry price's")]
    ExponentChanged,
    #[msg("The stake must be greater than zero")]
    ZeroStake,
    #[msg("The round no longer accepts positions")]
    JoinClosed,
    #[msg("The round has already been settled or voided")]
    RoundNotOpen,
    #[msg("The round has not reached its expiry")]
    SettleTooEarly,
    #[msg("The settle window has closed; the round can only be voided")]
    SettleWindowClosed,
    #[msg("The settle window is still open; the round cannot be voided yet")]
    SettleWindowOpen,
    #[msg("The price update was published before the moment it has to cover")]
    PriceTooEarly,
    #[msg(
        "The price update is not the first one published at or after the moment it has to cover"
    )]
    PriceNotFirst,
    #[msg("The price update was published too long after the moment it has to cover")]
    PriceTooLate,
    #[msg("The round's join deadline has not passed, so its entry price cannot be fixed yet")]
    LockTooEarly,
    #[msg("The round's entry price is already fixed")]
    AlreadyLocked,
    #[msg("The round's entry price has not been fixed")]
    RoundNotLocked,
    #[msg("The round has not been settled or voided")]
    RoundNotSettled,
    #[msg("The round still has positions that have not been claimed")]
    PositionsOutstanding,
    #[msg("Arithmetic overflow")]
    ArithmeticOverflow,
}
