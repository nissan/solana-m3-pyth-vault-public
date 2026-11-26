//! Signed price updates from Pyth's Hermes service, and the Pyth receiver instructions that
//! put one on chain and take its rent back.

use {
    crate::{chain::Chain, Res},
    anchor_lang::{
        prelude::Pubkey,
        solana_program::instruction::{AccountMeta, Instruction},
        system_program, AccountDeserialize,
    },
    pyth_solana_receiver_sdk::{config::Config, pda, PostUpdateParams},
    pythnet_sdk::wire::v1::{AccumulatorUpdateData, Proof},
    std::{thread::sleep, time::Duration},
};

const DEFAULT_HERMES: &str = "https://hermes.pyth.network";

/// Bytes an encoded VAA account holds before the signed message itself.
const ENCODED_VAA_HEADER: usize = 46;

/// Which of the receiver's treasuries collects the update fee. Any of them works.
const TREASURY_ID: u8 = 0;

/// One price update: what Hermes says it holds, and the signed bytes that prove it.
pub struct Update {
    pub price: i64,
    pub conf: u64,
    pub exponent: i32,
    pub publish_time: i64,
    pub prev_publish_time: i64,
    binary: Vec<u8>,
}

pub struct Hermes {
    base: String,
    key: String,
}

impl Hermes {
    pub fn from_env() -> Res<Self> {
        let key = std::env::var("PYTH_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .ok_or("PYTH_API_KEY is not set: Hermes has needed a key since 26 August 2026")?;
        let base = std::env::var("HERMES_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| DEFAULT_HERMES.to_string());
        Ok(Self { base, key })
    }

    fn get(&self, path: &str, feed_hex: &str) -> Result<Update, ureq::Error> {
        let url = format!(
            "{}/v2/updates/price/{path}?ids[]=0x{feed_hex}&encoding=hex&parsed=true",
            self.base
        );
        let text = ureq::get(&url)
            .header("Authorization", &format!("Bearer {}", self.key))
            .call()?
            .body_mut()
            .read_to_string()?;
        parse(&text).map_err(|e| ureq::Error::Other(e.to_string().into()))
    }

    /// The first update published at or after `unix_time`. Hermes may need a few seconds
    /// after that moment before it can serve it.
    pub fn at(&self, feed_hex: &str, unix_time: i64) -> Res<Update> {
        let mut last = None;
        for _ in 0..15 {
            match self.get(&unix_time.to_string(), feed_hex) {
                Ok(update) => return Ok(update),
                Err(error) => last = Some(error),
            }
            sleep(Duration::from_secs(3));
        }
        Err(format!(
            "Hermes has no update for unix time {unix_time}: {}",
            last.map(|e| e.to_string()).unwrap_or_default()
        )
        .into())
    }
}

fn parse(text: &str) -> Res<Update> {
    let json: serde_json::Value = serde_json::from_str(text)?;
    let parsed = &json["parsed"][0];
    let price = &parsed["price"];
    let number = |value: &serde_json::Value| -> Res<i128> {
        match value {
            serde_json::Value::String(s) => Ok(s.parse()?),
            serde_json::Value::Number(n) => Ok(n.as_i64().ok_or("not an integer")?.into()),
            _ => Err("Hermes' answer is missing a number".into()),
        }
    };
    Ok(Update {
        price: number(&price["price"])?.try_into()?,
        conf: number(&price["conf"])?.try_into()?,
        exponent: number(&price["expo"])?.try_into()?,
        publish_time: number(&price["publish_time"])?.try_into()?,
        prev_publish_time: number(&parsed["metadata"]["prev_publish_time"])?.try_into()?,
        binary: hex::decode(
            json["binary"]["data"][0]
                .as_str()
                .ok_or("Hermes' answer has no signed update")?,
        )?,
    })
}

/// An Anchor instruction's discriminator: the first eight bytes of `sha256("global:<name>")`.
fn discriminator(name: &str) -> Vec<u8> {
    solana_sha256_hasher::hash(format!("global:{name}").as_bytes()).to_bytes()[..8].to_vec()
}

/// The instructions that put one update on chain, fully verified, as two transactions:
/// together they are 51 bytes too large for one.
///
/// First the signed message goes into a new account of the Wormhole program the receiver
/// trusts, and that program checks its signatures against the signer set. Then the receiver
/// writes the price into `price_account`, and the message account is closed again. The
/// shorter `post_update_atomic` would leave the price marked as partially verified, which
/// the vault refuses.
///
/// `message_account` signs the first transaction and `price_account` the second, each with
/// the payer.
pub fn post_update_ixs(
    chain: &Chain,
    payer: &Pubkey,
    message_account: &Pubkey,
    price_account: &Pubkey,
    update: &Update,
) -> Res<(Vec<Instruction>, Vec<Instruction>)> {
    let Proof::WormholeMerkle { vaa, updates } =
        AccumulatorUpdateData::try_from_slice(&update.binary)
            .map_err(|_| "Hermes' signed update could not be decoded")?
            .proof;
    let vaa: Vec<u8> = vaa.into();
    let merkle_price_update = updates
        .into_iter()
        .next()
        .ok_or("Hermes' signed update holds no price")?;

    let config_address = pda::get_config_address();
    let config_account = chain
        .account(&config_address)?
        .ok_or("the Pyth receiver's config account is missing on this cluster")?;
    let wormhole = Config::try_deserialize(&mut config_account.data.as_slice())?.wormhole;
    // The signer set the message names, by its index.
    let signer_set_index: [u8; 4] = vaa
        .get(1..5)
        .ok_or("the signed update is too short")?
        .try_into()?;
    let signer_set =
        Pubkey::find_program_address(&[b"GuardianSet", &signer_set_index], &wormhole).0;

    let space = ENCODED_VAA_HEADER + vaa.len();
    // The system program's CreateAccount: index 0, lamports, space, owner.
    let mut create = 0u32.to_le_bytes().to_vec();
    create.extend(chain.rent(space)?.to_le_bytes());
    create.extend((space as u64).to_le_bytes());
    create.extend(wormhole.to_bytes());

    let mut write = discriminator("write_encoded_vaa");
    write.extend(0u32.to_le_bytes()); // offset in the message
    write.extend(anchor_lang::prelude::borsh::to_vec(&vaa)?);

    let mut post = discriminator("post_update");
    post.extend(anchor_lang::prelude::borsh::to_vec(&PostUpdateParams {
        merkle_price_update,
        treasury_id: TREASURY_ID,
    })?);

    let authority = AccountMeta::new_readonly(*payer, true);
    let message = AccountMeta::new(*message_account, false);
    let verify = vec![
        Instruction::new_with_bytes(
            system_program::ID,
            &create,
            vec![
                AccountMeta::new(*payer, true),
                AccountMeta::new(*message_account, true),
            ],
        ),
        Instruction::new_with_bytes(
            wormhole,
            &discriminator("init_encoded_vaa"),
            vec![authority.clone(), message.clone()],
        ),
        Instruction::new_with_bytes(wormhole, &write, vec![authority.clone(), message.clone()]),
        Instruction::new_with_bytes(
            wormhole,
            &discriminator("verify_encoded_vaa_v1"),
            vec![
                authority.clone(),
                message.clone(),
                AccountMeta::new_readonly(signer_set, false),
            ],
        ),
    ];
    let post = vec![
        Instruction::new_with_bytes(
            pyth_solana_receiver_sdk::ID,
            &post,
            vec![
                AccountMeta::new(*payer, true),
                AccountMeta::new_readonly(*message_account, false),
                AccountMeta::new_readonly(config_address, false),
                AccountMeta::new(pda::get_treasury_address(TREASURY_ID), false),
                AccountMeta::new(*price_account, true),
                AccountMeta::new_readonly(system_program::ID, false),
                // The write authority: who may overwrite or close the price account later.
                authority,
            ],
        ),
        Instruction::new_with_bytes(
            wormhole,
            &discriminator("close_encoded_vaa"),
            vec![AccountMeta::new(*payer, true), message],
        ),
    ];
    Ok((verify, post))
}

/// `reclaim_rent`: close a price account this payer posted and take its rent back.
pub fn reclaim_rent_ix(payer: &Pubkey, price_account: &Pubkey) -> Instruction {
    Instruction::new_with_bytes(
        pyth_solana_receiver_sdk::ID,
        &discriminator("reclaim_rent"),
        vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*price_account, false),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_from_hermes_is_parsed_with_its_previous_publish_time() {
        let text = r#"{"binary":{"encoding":"hex","data":["504e4155"]},"parsed":[{"id":"ef0d",
            "price":{"price":"7077356008","conf":"1994633","expo":-8,"publish_time":1795642769},
            "metadata":{"slot":35912855380,"proof_available_time":1795642769,
            "prev_publish_time":1795642768}}]}"#;
        let update = parse(text).unwrap();
        assert_eq!(update.price, 7_077_356_008);
        assert_eq!(update.conf, 1_994_633);
        assert_eq!(update.exponent, -8);
        assert_eq!(update.publish_time, 1_795_642_769);
        assert_eq!(update.prev_publish_time, 1_795_642_768);
        assert_eq!(update.binary, b"PNAU");
    }
}
