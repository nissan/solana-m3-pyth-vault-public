//! Shared LiteSVM fixtures for the price vault tests.
//!
//! Price updates are written straight into LiteSVM as accounts owned by the Pyth receiver
//! program, so each test controls the price, its confidence and its publish times exactly.
//! No test here proves that a real update from Pyth verifies; that is the devnet run's job.
//! Needs `anchor build --ignore-keys` first, so `target/deploy/price_vault.so` exists.
#![allow(dead_code)]
// litesvm returns its failure by value; the size is its choice, not ours.
#![allow(clippy::result_large_err)]

pub use anchor_lang::prelude::Pubkey;
use {
    anchor_lang::{
        solana_program::instruction::{AccountMeta, Instruction},
        system_program, AccountDeserialize, AccountSerialize, InstructionData, Space,
        ToAccountMetas,
    },
    litesvm::{types::TransactionResult, LiteSVM},
    price_vault::{
        error::VaultError,
        state::{Position, Round, Side, Vault, POSITION_SEED, ROUND_SEED, VAULT_SEED},
    },
    pyth_solana_receiver_sdk::price_update::{PriceFeedMessage, PriceUpdateV2, VerificationLevel},
    solana_account::Account,
    solana_clock::Clock,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::versioned::VersionedTransaction,
};

pub const SOL: u64 = 1_000_000_000;

/// The clock every test starts at.
pub const T0: i64 = 1_800_000_000;
pub const JOIN_DEADLINE: i64 = T0 + 100;
pub const EXPIRY: i64 = T0 + 200;
pub const ROUND_ID: u64 = 7;

/// SOL/USD on Pyth. Any 32 bytes would do here; the real id keeps the fixture honest.
pub const FEED: [u8; 32] = [
    0xef, 0x0d, 0x8b, 0x6f, 0xda, 0x2c, 0xeb, 0xa4, 0x1d, 0xa1, 0x5d, 0x40, 0x95, 0xd1, 0xda, 0x39,
    0x2a, 0x0d, 0x2f, 0x8e, 0xd0, 0xc6, 0xc7, 0xbc, 0x0f, 0x4c, 0xfa, 0xc8, 0xc2, 0x80, 0xb5, 0x6d,
];
pub const OTHER_FEED: [u8; 32] = [9; 32];

/// 150.00 with exponent -8, and a confidence of 0.05% of it.
pub const ENTRY_PRICE: i64 = 150_00000000;
pub const CONF: u64 = 7_500_000;
pub const EXPONENT: i32 = -8;

/// Anchor's built-in error codes the tests expect.
pub const ANCHOR_CONSTRAINT_HAS_ONE: u32 = 2001;
pub const ANCHOR_CONSTRAINT_SEEDS: u32 = 2006;
pub const ANCHOR_ACCOUNT_OWNED_BY_WRONG_PROGRAM: u32 = 3007;
pub const ANCHOR_ACCOUNT_NOT_SIGNER: u32 = 3010;
pub const ANCHOR_ACCOUNT_NOT_INITIALIZED: u32 = 3012;
/// The system program's "account already in use".
pub const SYSTEM_ACCOUNT_ALREADY_IN_USE: u32 = 0;

/// Custom error code for a `VaultError` variant (Anchor numbers them from 6000).
pub fn code(e: VaultError) -> u32 {
    6000 + e as u32
}

pub fn round_pda(opener: &Pubkey, round_id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[ROUND_SEED, opener.as_ref(), &round_id.to_le_bytes()],
        &price_vault::id(),
    )
    .0
}

pub fn vault_pda(round: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[VAULT_SEED, round.as_ref()], &price_vault::id()).0
}

pub fn position_pda(round: &Pubkey, player: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[POSITION_SEED, round.as_ref(), player.as_ref()],
        &price_vault::id(),
    )
    .0
}

/// A fully verified update for `FEED` with the default confidence.
pub fn update(price: i64, publish_time: i64, prev_publish_time: i64) -> PriceUpdateV2 {
    PriceUpdateV2 {
        write_authority: Pubkey::new_unique(),
        verification_level: VerificationLevel::Full,
        price_message: PriceFeedMessage {
            feed_id: FEED,
            price,
            conf: CONF,
            exponent: EXPONENT,
            publish_time,
            prev_publish_time,
            ema_price: price,
            ema_conf: CONF,
        },
        posted_slot: 1,
    }
}

/// The one update that covers the default round's join deadline, which fixes its entry price.
pub fn entry_update(price: i64) -> PriceUpdateV2 {
    update(price, JOIN_DEADLINE, JOIN_DEADLINE - 1)
}

/// The one update that covers the default round's expiry: published at the expiry, with
/// the update before it published earlier.
pub fn exit_update(price: i64) -> PriceUpdateV2 {
    update(price, EXPIRY, EXPIRY - 1)
}

pub struct Env {
    pub svm: LiteSVM,
    pub opener: Keypair,
    pub alice: Keypair,
    pub bob: Keypair,
    pub carol: Keypair,
    /// Pays every transaction fee, so the other accounts' balances change only by what the
    /// program moves.
    pub relayer: Keypair,
}

impl Env {
    pub fn new() -> Self {
        let mut svm = LiteSVM::new();
        // Read at run time, not with include_bytes!: `anchor build` compiles the tests while
        // generating the IDL, before the program binary exists.
        let so_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/deploy/price_vault.so"
        );
        let bytes = std::fs::read(so_path)
            .unwrap_or_else(|e| panic!("{so_path}: {e}; run `anchor build --ignore-keys` first"));
        svm.add_program(price_vault::id(), &bytes).unwrap();

        let env = Env {
            svm,
            opener: Keypair::new(),
            alice: Keypair::new(),
            bob: Keypair::new(),
            carol: Keypair::new(),
            relayer: Keypair::new(),
        };
        let mut env = env;
        for key in [
            env.opener.pubkey(),
            env.alice.pubkey(),
            env.bob.pubkey(),
            env.carol.pubkey(),
            env.relayer.pubkey(),
        ] {
            env.svm.airdrop(&key, 100 * SOL).unwrap();
        }
        env.set_time(T0);
        env
    }

    pub fn set_time(&mut self, unix_timestamp: i64) {
        let mut clock = self.svm.get_sysvar::<Clock>();
        clock.unix_timestamp = unix_timestamp;
        self.svm.set_sysvar(&clock);
    }

    // ---- accounts --------------------------------------------------------------------

    /// Write a rent-exempt account with the given owner and data.
    pub fn put(&mut self, address: Pubkey, owner: Pubkey, data: Vec<u8>) {
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        let account = Account {
            lamports,
            data,
            owner,
            executable: false,
            rent_epoch: 0,
        };
        self.svm.set_account(address, account).unwrap();
    }

    /// Post a price update the way the Pyth receiver would leave it: an account it owns.
    pub fn post(&mut self, update: PriceUpdateV2) -> Pubkey {
        self.post_owned_by(update, pyth_solana_receiver_sdk::ID)
    }

    pub fn post_owned_by(&mut self, update: PriceUpdateV2, owner: Pubkey) -> Pubkey {
        let address = Pubkey::new_unique();
        let mut data = Vec::new();
        update.try_serialize(&mut data).unwrap();
        self.put(address, owner, data);
        address
    }

    pub fn account(&self, address: &Pubkey) -> Option<Account> {
        self.svm.get_account(address).filter(|a| a.lamports > 0)
    }

    pub fn lamports(&self, address: &Pubkey) -> u64 {
        self.svm.get_account(address).map_or(0, |a| a.lamports)
    }

    /// Every listed account as it is now, to compare after a transaction that must change
    /// nothing.
    pub fn snapshot(&self, addresses: &[Pubkey]) -> Vec<Option<Account>> {
        addresses.iter().map(|a| self.account(a)).collect()
    }

    pub fn round(&self) -> Pubkey {
        round_pda(&self.opener.pubkey(), ROUND_ID)
    }

    pub fn vault(&self) -> Pubkey {
        vault_pda(&self.round())
    }

    pub fn round_state(&self, round: &Pubkey) -> Round {
        let account = self.account(round).expect("round account exists");
        Round::try_deserialize(&mut account.data.as_slice()).unwrap()
    }

    /// Overwrite a round's stored state, keeping its lamports. For states no instruction
    /// sequence can reach in a test, such as totals near `u64::MAX`.
    pub fn set_round_state(&mut self, round: &Pubkey, state: &Round) {
        let mut account = self.account(round).expect("round account exists");
        let mut data = Vec::new();
        state.try_serialize(&mut data).unwrap();
        assert_eq!(data.len(), account.data.len());
        account.data = data;
        self.svm.set_account(*round, account).unwrap();
    }

    pub fn position_state(&self, round: &Pubkey, player: &Pubkey) -> Position {
        let account = self
            .account(&position_pda(round, player))
            .expect("position account exists");
        Position::try_deserialize(&mut account.data.as_slice()).unwrap()
    }

    pub fn vault_rent(&self) -> u64 {
        self.svm
            .minimum_balance_for_rent_exemption(8 + Vault::INIT_SPACE)
    }

    pub fn position_rent(&self) -> u64 {
        self.svm
            .minimum_balance_for_rent_exemption(8 + Position::INIT_SPACE)
    }

    /// Stake held by a round's vault: its lamports above its own rent.
    pub fn pool(&self, round: &Pubkey) -> u64 {
        self.lamports(&vault_pda(round)) - self.vault_rent()
    }

    // ---- sending ---------------------------------------------------------------------

    /// Send one instruction. The relayer pays the fee; `signers` sign as well.
    pub fn send(&mut self, ix: Instruction, signers: &[&Keypair]) -> TransactionResult {
        // A fresh blockhash per transaction so a repeated instruction is not rejected as
        // already processed.
        self.svm.expire_blockhash();
        let blockhash = self.svm.latest_blockhash();
        let relayer = self.relayer.insecure_clone();
        let msg = Message::new_with_blockhash(&[ix], Some(&relayer.pubkey()), &blockhash);
        let mut all: Vec<&Keypair> = vec![&relayer];
        all.extend(signers.iter().filter(|k| k.pubkey() != relayer.pubkey()));
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &all).unwrap();
        self.svm.send_transaction(tx)
    }

    // ---- instruction builders --------------------------------------------------------

    pub fn open_ix(
        &self,
        opener: &Pubkey,
        round_id: u64,
        feed_id: [u8; 32],
        join_deadline: i64,
        expiry: i64,
    ) -> Instruction {
        let round = round_pda(opener, round_id);
        Instruction::new_with_bytes(
            price_vault::id(),
            &price_vault::instruction::OpenRound {
                round_id,
                feed_id,
                join_deadline,
                expiry,
            }
            .data(),
            price_vault::accounts::OpenRound {
                opener: *opener,
                round,
                vault: vault_pda(&round),
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        )
    }

    pub fn open_default_ix(&self) -> Instruction {
        self.open_ix(&self.opener.pubkey(), ROUND_ID, FEED, JOIN_DEADLINE, EXPIRY)
    }

    /// Open the default round. It has no entry price until it is locked.
    pub fn open_default(&mut self) -> Pubkey {
        let opener = self.opener.insecure_clone();
        let ix = self.open_default_ix();
        self.send(ix, &[&opener])
            .expect("open_round should succeed");
        self.round()
    }

    pub fn lock_ix(&self, round: &Pubkey, price_update: Pubkey) -> Instruction {
        Instruction::new_with_bytes(
            price_vault::id(),
            &price_vault::instruction::LockRound {}.data(),
            price_vault::accounts::LockRound {
                round: *round,
                price_update,
            }
            .to_account_metas(None),
        )
    }

    /// Move the clock to the join deadline and lock the default round at `entry_price`.
    pub fn lock_at(&mut self, entry_price: i64) {
        self.set_time(JOIN_DEADLINE);
        let entry = self.post(entry_update(entry_price));
        let ix = self.lock_ix(&self.round(), entry);
        self.send(ix, &[]).expect("lock_round should succeed");
    }

    pub fn join_accounts(&self, round: &Pubkey, player: &Pubkey) -> price_vault::accounts::Join {
        price_vault::accounts::Join {
            player: *player,
            round: *round,
            vault: vault_pda(round),
            position: position_pda(round, player),
            system_program: system_program::ID,
        }
    }

    pub fn join_ix_with(
        &self,
        accounts: price_vault::accounts::Join,
        side: Side,
        amount: u64,
    ) -> Instruction {
        Instruction::new_with_bytes(
            price_vault::id(),
            &price_vault::instruction::Join { side, amount }.data(),
            accounts.to_account_metas(None),
        )
    }

    pub fn join_ix(&self, round: &Pubkey, player: &Pubkey, side: Side, amount: u64) -> Instruction {
        self.join_ix_with(self.join_accounts(round, player), side, amount)
    }

    pub fn join(&mut self, round: &Pubkey, player: &Keypair, side: Side, amount: u64) {
        let ix = self.join_ix(round, &player.pubkey(), side, amount);
        self.send(ix, &[player]).expect("join should succeed");
    }

    pub fn settle_ix(&self, round: &Pubkey, price_update: Pubkey) -> Instruction {
        Instruction::new_with_bytes(
            price_vault::id(),
            &price_vault::instruction::Settle {}.data(),
            price_vault::accounts::Settle {
                round: *round,
                price_update,
            }
            .to_account_metas(None),
        )
    }

    /// Lock the default round at `ENTRY_PRICE` if nobody has, then move the clock to the
    /// expiry and settle it at `exit_price`.
    pub fn settle_at(&mut self, exit_price: i64) {
        if !self.round_state(&self.round()).locked {
            self.lock_at(ENTRY_PRICE);
        }
        self.set_time(EXPIRY);
        let exit = self.post(exit_update(exit_price));
        let ix = self.settle_ix(&self.round(), exit);
        self.send(ix, &[]).expect("settle should succeed");
    }

    pub fn void_ix(&self, round: &Pubkey) -> Instruction {
        Instruction::new_with_bytes(
            price_vault::id(),
            &price_vault::instruction::VoidRound {}.data(),
            price_vault::accounts::VoidRound { round: *round }.to_account_metas(None),
        )
    }

    pub fn claim_accounts(&self, round: &Pubkey, player: &Pubkey) -> price_vault::accounts::Claim {
        price_vault::accounts::Claim {
            round: *round,
            vault: vault_pda(round),
            position: position_pda(round, player),
            player: *player,
        }
    }

    pub fn claim_ix_with(&self, accounts: price_vault::accounts::Claim) -> Instruction {
        Instruction::new_with_bytes(
            price_vault::id(),
            &price_vault::instruction::Claim {}.data(),
            accounts.to_account_metas(None),
        )
    }

    pub fn claim_ix(&self, round: &Pubkey, player: &Pubkey) -> Instruction {
        self.claim_ix_with(self.claim_accounts(round, player))
    }

    /// Claim for `player`. Nobody but the relayer signs: claim is permissionless.
    pub fn claim(&mut self, round: &Pubkey, player: &Pubkey) {
        let ix = self.claim_ix(round, player);
        self.send(ix, &[]).expect("claim should succeed");
    }

    pub fn close_ix(&self, round: &Pubkey, opener: &Pubkey) -> Instruction {
        Instruction::new_with_bytes(
            price_vault::id(),
            &price_vault::instruction::CloseRound {}.data(),
            price_vault::accounts::CloseRound {
                opener: *opener,
                round: *round,
                vault: vault_pda(round),
            }
            .to_account_metas(None),
        )
    }
}

/// The same instruction with one account no longer marked as a signer.
pub fn without_signer(mut ix: Instruction, account: &Pubkey) -> Instruction {
    let meta: &mut AccountMeta = ix
        .accounts
        .iter_mut()
        .find(|m| m.pubkey == *account)
        .expect("account is in the instruction");
    meta.is_signer = false;
    ix
}

/// Assert the transaction failed in its first instruction with `InstructionError::Custom(code)`.
pub fn assert_custom_error(result: TransactionResult, expected: u32) {
    let failed = match result {
        Ok(meta) => panic!(
            "expected custom error {expected}, but it succeeded: {:#?}",
            meta.logs
        ),
        Err(failed) => failed,
    };
    let text = format!("{:?}", failed.err);
    let want = format!("InstructionError(0, Custom({expected}))");
    assert_eq!(text, want, "logs: {:#?}", failed.meta.logs);
}
