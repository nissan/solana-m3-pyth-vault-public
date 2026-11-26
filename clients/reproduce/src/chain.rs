//! Reading from and sending to the cluster, with the rules a devnet run has to keep:
//! refuse any cluster but devnet or a local validator, print each signature before sending
//! it, wait for `finalized`, and never resend under a new signature.

use {
    crate::Res,
    anchor_lang::{prelude::Pubkey, solana_program::instruction::Instruction},
    solana_commitment_config::{CommitmentConfig, CommitmentLevel},
    solana_keypair::Keypair,
    solana_rpc_client::rpc_client::RpcClient,
    solana_rpc_client_api::config::{RpcSendTransactionConfig, RpcSimulateTransactionConfig},
    solana_signature::Signature,
    solana_signer::Signer,
    solana_transaction::Transaction,
    std::{
        cell::Cell,
        sync::LazyLock,
        thread::sleep,
        time::{Duration, Instant},
    },
};

const DEVNET_GENESIS: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
const DEFAULT_RPC: &str = "https://api.devnet.solana.com";
const CLOCK_SYSVAR: Pubkey = anchor_lang::pubkey!("SysvarC1ock11111111111111111111111111111111");

/// The RPC address. It may carry a key, so it is never printed: see `redact`.
static RPC_URL: LazyLock<String> = LazyLock::new(|| {
    std::env::var("SOLANA_RPC_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_RPC.to_string())
});

/// Remove the RPC address, and anything after a `?` in it, from text about to be printed.
pub fn redact(text: &str) -> String {
    let url = &*RPC_URL;
    let mut out = text.replace(url.as_str(), "<rpc address>");
    if let Some((_, query)) = url.split_once('?') {
        if !query.is_empty() {
            out = out.replace(query, "<removed>");
        }
        if let Some((_, value)) = query.split_once('=') {
            if !value.is_empty() {
                out = out.replace(value, "<removed>");
            }
        }
    }
    out
}

/// Read a keypair file named by an environment variable. Only the variable's name appears
/// in an error, never the path's contents.
pub fn read_keypair(variable: &str) -> Res<Keypair> {
    let path = std::env::var(variable)
        .map_err(|_| format!("{variable} is not set: it names the funded keypair file"))?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{variable} names a file that cannot be read: {}", e.kind()))?;
    let bytes: Vec<u8> = serde_json::from_str(&text)
        .map_err(|_| format!("{variable} does not name a Solana keypair file"))?;
    Ok(Keypair::try_from(bytes.as_slice())
        .map_err(|_| format!("{variable} does not name a Solana keypair file"))?)
}

pub struct Simulated {
    /// The custom program error the simulation ended with, if it ended with one.
    pub custom_error: Option<u32>,
    logs: Vec<String>,
}

impl Simulated {
    /// Whether the program logged this error name.
    pub fn logs_name(&self, name: &str) -> bool {
        let needle = format!("Error Code: {name}.");
        self.logs.iter().any(|line| line.contains(&needle))
    }
}

pub struct Chain {
    client: RpcClient,
    host: String,
    local: bool,
    sent: Cell<u32>,
}

impl Chain {
    pub fn from_env() -> Res<Self> {
        let url = RPC_URL.clone();
        let host = url
            .split_once("://")
            .map_or(url.as_str(), |(_, rest)| rest)
            .split(['/', '?'])
            .next()
            .unwrap_or_default()
            .to_string();
        let local = host.starts_with("127.0.0.1") || host.starts_with("localhost");
        let client = RpcClient::new_with_commitment(url, CommitmentConfig::finalized());
        let genesis = client.get_genesis_hash()?.to_string();
        // Devnet by its genesis hash; anything else only when it is on this machine.
        if genesis != DEVNET_GENESIS && !local {
            return Err(format!("Refusing to run: genesis hash {genesis} is not devnet's").into());
        }
        Ok(Self {
            client,
            host,
            local: genesis != DEVNET_GENESIS,
            sent: Cell::new(0),
        })
    }

    pub fn describe(&self) -> String {
        if self.local {
            format!("local validator at {}", self.host)
        } else {
            format!("devnet (genesis {DEVNET_GENESIS}) through {}", self.host)
        }
    }

    pub fn sent(&self) -> u32 {
        self.sent.get()
    }

    // ---- reads, all at `finalized` -------------------------------------------------------

    pub fn account(&self, address: &Pubkey) -> Res<Option<solana_account::Account>> {
        Ok(self
            .client
            .get_account_with_commitment(address, CommitmentConfig::finalized())?
            .value)
    }

    pub fn accounts(&self, addresses: &[Pubkey]) -> Res<Vec<Option<solana_account::Account>>> {
        Ok(self
            .client
            .get_multiple_accounts_with_commitment(addresses, CommitmentConfig::finalized())?
            .value)
    }

    pub fn balance(&self, address: &Pubkey) -> Res<u64> {
        Ok(self
            .client
            .get_balance_with_commitment(address, CommitmentConfig::finalized())?
            .value)
    }

    pub fn rent(&self, data_len: usize) -> Res<u64> {
        Ok(self
            .client
            .get_minimum_balance_for_rent_exemption(data_len)?)
    }

    /// The cluster's own clock, which is what the program compares prices and deadlines to.
    pub fn unix_time(&self) -> Res<i64> {
        let clock = self
            .client
            .get_account_with_commitment(&CLOCK_SYSVAR, CommitmentConfig::confirmed())?
            .value
            .ok_or("the clock sysvar is missing")?;
        let bytes: [u8; 8] = clock.data[32..40].try_into()?;
        Ok(i64::from_le_bytes(bytes))
    }

    // ---- sending ------------------------------------------------------------------------

    fn signed(
        &self,
        instructions: &[Instruction],
        payer: &Keypair,
        signers: &[&Keypair],
    ) -> Res<(Transaction, u64)> {
        let (blockhash, last_valid_height) = self
            .client
            .get_latest_blockhash_with_commitment(CommitmentConfig::confirmed())?;
        let mut all: Vec<&Keypair> = vec![payer];
        all.extend(signers);
        let tx = Transaction::new_signed_with_payer(
            instructions,
            Some(&payer.pubkey()),
            &all,
            blockhash,
        );
        Ok((tx, last_valid_height))
    }

    /// Sign, print the signature, send, and wait until the transaction is finalized.
    ///
    /// While waiting, the same signed transaction is rebroadcast: that cannot run it twice.
    /// If its blockhash expires before it lands, this stops and names the signature, so the
    /// next step is to look that signature up, not to send again.
    pub fn send(
        &self,
        label: &str,
        instructions: &[Instruction],
        payer: &Keypair,
        signers: &[&Keypair],
    ) -> Res<()> {
        self.send_until(
            label,
            instructions,
            payer,
            signers,
            CommitmentConfig::finalized(),
        )
        .map(|_| ())
    }

    /// As `send`, but returns at `confirmed`. Only for a transaction whose successor cannot
    /// succeed without it; the caller checks it with `expect_finalized` afterwards.
    pub fn send_confirmed(
        &self,
        label: &str,
        instructions: &[Instruction],
        payer: &Keypair,
        signers: &[&Keypair],
    ) -> Res<Signature> {
        self.send_until(
            label,
            instructions,
            payer,
            signers,
            CommitmentConfig::confirmed(),
        )
    }

    pub fn expect_finalized(&self, signature: &Signature) -> Res<()> {
        let status = self.client.get_signature_statuses(&[*signature])?.value[0].clone();
        match status {
            Some(status)
                if status.err.is_none()
                    && status.satisfies_commitment(CommitmentConfig::finalized()) =>
            {
                Ok(())
            }
            _ => Err(format!("{signature} is not finalized without error").into()),
        }
    }

    fn send_until(
        &self,
        label: &str,
        instructions: &[Instruction],
        payer: &Keypair,
        signers: &[&Keypair],
        commitment: CommitmentConfig,
    ) -> Res<Signature> {
        let (tx, last_valid_height) = self.signed(instructions, payer, signers)?;
        let signature = tx.signatures[0];
        println!("   send  {label}");
        println!("         {signature}");
        if !self.local {
            println!("         https://explorer.solana.com/tx/{signature}?cluster=devnet");
        }

        // The first send simulates first, so a program error surfaces here with its logs.
        self.client.send_transaction_with_config(
            &tx,
            RpcSendTransactionConfig {
                preflight_commitment: Some(CommitmentLevel::Confirmed),
                max_retries: Some(0),
                ..RpcSendTransactionConfig::default()
            },
        )?;
        self.sent.set(self.sent.get() + 1);

        let started = Instant::now();
        let mut last_broadcast = Instant::now();
        loop {
            sleep(Duration::from_secs(2));
            if let Some(status) = self.client.get_signature_statuses(&[signature])?.value[0].clone()
            {
                if let Some(error) = status.err {
                    return Err(format!("{signature} failed on chain: {error:?}").into());
                }
                if status.satisfies_commitment(commitment) {
                    println!(
                        "         {} in slot {} after {} s",
                        if commitment.is_finalized() {
                            "finalized"
                        } else {
                            "confirmed"
                        },
                        status.slot,
                        started.elapsed().as_secs()
                    );
                    return Ok(signature);
                }
                continue;
            }
            if self.client.get_block_height()? > last_valid_height {
                return Err(format!(
                    "{signature} did not land before its blockhash expired. \
                     Look the signature up before sending anything again."
                )
                .into());
            }
            if last_broadcast.elapsed() > Duration::from_secs(6) {
                last_broadcast = Instant::now();
                // Same bytes, same signature. A failure here is not the transaction's.
                let _ = self.client.send_transaction_with_config(
                    &tx,
                    RpcSendTransactionConfig {
                        skip_preflight: true,
                        max_retries: Some(0),
                        ..RpcSendTransactionConfig::default()
                    },
                );
            }
        }
    }

    /// Sign a transaction and simulate it with signature verification on. Nothing is sent.
    pub fn simulate(&self, instructions: &[Instruction], payer: &Keypair) -> Res<Simulated> {
        let (tx, _) = self.signed(instructions, payer, &[])?;
        let result = self
            .client
            .simulate_transaction_with_config(
                &tx,
                RpcSimulateTransactionConfig {
                    sig_verify: true,
                    commitment: Some(CommitmentConfig::confirmed()),
                    ..RpcSimulateTransactionConfig::default()
                },
            )?
            .value;
        // The error's exact type differs between client versions; its text does not.
        let text = result.err.map(|e| format!("{e:?}")).unwrap_or_default();
        let text = text
            .strip_prefix("UiTransactionError(")
            .and_then(|t| t.strip_suffix(")"))
            .unwrap_or(&text)
            .to_string();
        let custom_error = text
            .split_once("Custom(")
            .and_then(|(_, rest)| rest.split_once(')'))
            .and_then(|(digits, _)| digits.parse().ok());
        let logs = result.logs.unwrap_or_default();
        println!(
            "   simulated, not sent: {}",
            if text.is_empty() { "no error" } else { &text }
        );
        Ok(Simulated { custom_error, logs })
    }
}
