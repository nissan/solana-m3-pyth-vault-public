//! The checks every price goes through before the program uses it.

use {
    crate::{
        error::VaultError,
        state::{MAX_CONF_BPS, MAX_PRICE_AGE_SECS},
    },
    anchor_lang::prelude::*,
    pyth_solana_receiver_sdk::price_update::{PriceUpdateV2, VerificationLevel},
};

pub struct Reading {
    pub price: i64,
    pub conf: u64,
    pub exponent: i32,
    pub publish_time: i64,
    pub prev_publish_time: i64,
}

/// Read `feed_id`'s price from a price update account.
///
/// The account type has already proven the Pyth receiver program owns it. This checks that
/// the receiver verified the update in full and not partially, that it is for the round's
/// feed, that the price is positive and that its confidence interval is within
/// `MAX_CONF_BPS`. It does not judge the publish time: `read_covering` does.
pub fn read(update: &PriceUpdateV2, feed_id: &[u8; 32]) -> Result<Reading> {
    require!(
        update.verification_level == VerificationLevel::Full,
        VaultError::PriceNotFullyVerified
    );
    let message = &update.price_message;
    require!(message.feed_id == *feed_id, VaultError::WrongFeed);
    require!(message.price > 0, VaultError::NonPositivePrice);
    // conf / price <= MAX_CONF_BPS / 10_000, cross-multiplied. Both products fit in u128:
    // each is a 64-bit value times a small constant.
    let conf_scaled = u128::from(message.conf) * 10_000;
    let limit = u128::from(message.price.unsigned_abs()) * u128::from(MAX_CONF_BPS);
    require!(conf_scaled <= limit, VaultError::ConfidenceTooWide);
    Ok(Reading {
        price: message.price,
        conf: message.conf,
        exponent: message.exponent,
        publish_time: message.publish_time,
        prev_publish_time: message.prev_publish_time,
    })
}

/// Read the one price that covers `moment`: the first published at or after it.
///
/// Exactly one update satisfies `prev_publish_time < moment <= publish_time`, so whoever
/// brings the update cannot pick a time or an update that suits their side. An update more
/// than `MAX_PRICE_AGE_SECS` after the moment means the feed went quiet across it and says
/// little about the price then; it is refused, and the round ends in a refund.
pub fn read_covering(update: &PriceUpdateV2, feed_id: &[u8; 32], moment: i64) -> Result<Reading> {
    let reading = read(update, feed_id)?;
    require!(reading.publish_time >= moment, VaultError::PriceTooEarly);
    require!(
        reading.prev_publish_time < moment,
        VaultError::PriceNotFirst
    );
    let latest = moment
        .checked_add(MAX_PRICE_AGE_SECS)
        .ok_or(VaultError::ArithmeticOverflow)?;
    require!(reading.publish_time <= latest, VaultError::PriceTooLate);
    Ok(reading)
}
