use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::Utc;

use log::info;
use near_primitives::crypto::signature::{PublicKey, Signature};
use near_primitives::crypto::signer::InMemorySigner;
use near_primitives::hash::{hash, CryptoHash};
use near_primitives::rpc::{AccountViewCallResult, QueryResponse};
use near_primitives::test_utils::get_public_key_from_seed;
use near_primitives::transaction::{
    AsyncCall, ReceiptTransaction, SignedTransaction, TransactionResult, TransactionStatus,
};
use near_primitives::types::{AccountId, BlockIndex, MerkleHash, ShardId, ValidatorStake};
use near_store::test_utils::create_test_store;
use near_store::{Store, StoreUpdate, Trie, TrieChanges, WrappedTrieChanges};

use crate::error::{Error, ErrorKind};
use crate::types::{BlockHeader, ReceiptResult, RuntimeAdapter, Weight};
use crate::{Chain, ValidTransaction};
use near_primitives::merkle::merklize;
use near_primitives::sharding::ShardChunkHeader;
use near_primitives::transaction::ReceiptBody::NewCall;
use near_primitives::transaction::TransactionBody::SendMoney;

/// Simple key value runtime for tests.
pub struct KeyValueRuntime {
    store: Arc<Store>,
    trie: Arc<Trie>,
    root: MerkleHash,
    validators: Vec<ValidatorStake>,
    validators_per_shard: u64,

    // A mapping state_root => {account id => amounts}, for transactions and receipts
    amounts: RwLock<HashMap<MerkleHash, HashMap<AccountId, u128>>>,
}

pub fn account_id_to_shard_id(account_id: &AccountId, num_shards: ShardId) -> ShardId {
    ((hash(&account_id.clone().into_bytes()).0).0[0] as u64) % num_shards
}

impl KeyValueRuntime {
    pub fn new(store: Arc<Store>) -> Self {
        Self::new_with_validators(store, vec!["test".to_string()], 1)
    }

    pub fn new_with_validators(
        store: Arc<Store>,
        validators: Vec<AccountId>,
        validators_per_shard: u64,
    ) -> Self {
        let trie = Arc::new(Trie::new(store.clone()));
        let mut initial_amounts = HashMap::new();
        for (i, validator) in validators.iter().enumerate() {
            initial_amounts.insert(validator.clone(), (1000 + 100 * i) as u128);
        }

        let mut amounts = HashMap::new();
        amounts.insert(MerkleHash::default(), initial_amounts);
        KeyValueRuntime {
            store,
            trie,
            root: MerkleHash::default(),
            validators: validators
                .iter()
                .map(|account_id| ValidatorStake {
                    account_id: account_id.clone(),
                    public_key: get_public_key_from_seed(account_id),
                    amount: 1_000_000,
                })
                .collect(),
            validators_per_shard,
            amounts: RwLock::new(amounts),
        }
    }

    pub fn get_root(&self) -> MerkleHash {
        self.root
    }
}

impl RuntimeAdapter for KeyValueRuntime {
    fn genesis_state(&self) -> (StoreUpdate, Vec<MerkleHash>) {
        (
            self.store.store_update(),
            ((0..self.num_shards()).map(|_| MerkleHash::default()).collect()),
        )
    }

    fn compute_block_weight(
        &self,
        prev_header: &BlockHeader,
        header: &BlockHeader,
    ) -> Result<Weight, Error> {
        let validator = &self.validators[(header.height as usize) % self.validators.len()];
        if !header.verify_block_producer(&validator.public_key) {
            return Err(ErrorKind::InvalidBlockProposer.into());
        }
        Ok(prev_header.total_weight.next(header.approval_sigs.len() as u64))
    }

    fn verify_chunk_header_signature(&self, _header: &ShardChunkHeader) -> bool {
        true
    }

    fn get_epoch_block_proposers(
        &self,
        _parent_hash: CryptoHash,
        _height: BlockIndex,
    ) -> Result<Vec<(AccountId, u64)>, Box<dyn std::error::Error>> {
        Ok(self.validators.iter().map(|x| (x.account_id.clone(), 1)).collect())
    }

    fn get_block_proposer(
        &self,
        _parent_hash: CryptoHash,
        height: BlockIndex,
    ) -> Result<AccountId, Box<dyn std::error::Error>> {
        Ok(self.validators[(height as usize) % self.validators.len()].account_id.clone())
    }

    fn get_chunk_proposer(
        &self,
        _parent_hash: CryptoHash,
        height: BlockIndex,
        shard_id: ShardId,
    ) -> Result<AccountId, Box<dyn std::error::Error>> {
        assert_eq!((self.validators.len() as u64) % self.num_shards(), 0);
        let validators_per_shard = self.validators_per_shard;
        let offset = (shard_id / validators_per_shard * validators_per_shard) as usize;
        // The +1 is so that if all validators validate all shards in a test, the chunk producer
        //     doesn't match the block producer
        let delta = ((shard_id + height + 1) % validators_per_shard) as usize;
        Ok(self.validators[offset + delta].account_id.clone())
    }

    fn check_validator_signature(&self, _account_id: &AccountId, _signature: &Signature) -> bool {
        true
    }

    fn num_shards(&self) -> ShardId {
        if self.validators.len() < 64 || self.validators.len() % 4 != 0 {
            self.validators.len() as ShardId
        } else {
            (self.validators.len() / 4) as ShardId
        }
    }

    fn num_total_parts(&self, _parent_hash: CryptoHash, _height: BlockIndex) -> usize {
        48
    }

    fn num_data_parts(&self, _parent_hash: CryptoHash, _height: BlockIndex) -> usize {
        16
    }

    fn account_id_to_shard_id(&self, account_id: &AccountId) -> ShardId {
        account_id_to_shard_id(account_id, self.num_shards())
    }

    fn get_part_owner(
        &self,
        _parent_hash: CryptoHash,
        _height: BlockIndex,
        part_id: u64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        Ok(self.validators[part_id as usize % self.validators.len()].account_id.clone())
    }

    fn cares_about_shard(
        &self,
        account_id: &AccountId,
        _parent_hash: CryptoHash,
        _height: BlockIndex,
        shard_id: ShardId,
    ) -> bool {
        assert_eq!((self.validators.len() as u64) % self.num_shards(), 0);
        let validators_per_shard = self.validators_per_shard;
        let offset = (shard_id / validators_per_shard * validators_per_shard) as usize;
        for validator in self.validators[offset..offset + (validators_per_shard as usize)].iter() {
            if validator.account_id == *account_id {
                return true;
            }
        }
        false
    }

    fn validate_tx(
        &self,
        _shard_id: ShardId,
        _state_root: MerkleHash,
        transaction: SignedTransaction,
    ) -> Result<ValidTransaction, String> {
        Ok(ValidTransaction { transaction })
    }

    fn add_validator_proposals(
        &self,
        _parent_hash: CryptoHash,
        _current_hash: CryptoHash,
        _block_index: u64,
        _proposals: Vec<ValidatorStake>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    fn apply_transactions(
        &self,
        shard_id: ShardId,
        state_root: &MerkleHash,
        _block_index: BlockIndex,
        _prev_block_hash: &CryptoHash,
        receipts: &Vec<ReceiptTransaction>,
        transactions: &Vec<SignedTransaction>,
    ) -> Result<
        (
            WrappedTrieChanges,
            MerkleHash,
            Vec<TransactionResult>,
            ReceiptResult,
            Vec<ValidatorStake>,
        ),
        Box<dyn std::error::Error>,
    > {
        let mut tx_results = vec![];

        let mut accounts_mapping =
            self.amounts.read().unwrap().get(state_root).cloned().unwrap_or_else(|| HashMap::new());

        let mut balance_transfers = vec![];

        for receipt in receipts.iter() {
            if let NewCall(call) = &receipt.body {
                info!(
                    "MOO applying receipt in shard {} from {} to {} amt {}",
                    shard_id, receipt.originator, receipt.receiver, call.amount
                );
                assert_eq!(self.account_id_to_shard_id(&receipt.receiver), shard_id);
                balance_transfers.push((
                    receipt.originator.clone(),
                    receipt.receiver.clone(),
                    call.amount,
                ));
            } else {
                unreachable!();
            }
        }

        for transaction in transactions {
            if let SendMoney(send_money_tx) = &transaction.body {
                info!(
                    "MOO applying transaction from {} to {} amt {}",
                    send_money_tx.originator, send_money_tx.receiver, send_money_tx.amount
                );
                assert_eq!(self.account_id_to_shard_id(&send_money_tx.originator), shard_id);
                balance_transfers.push((
                    send_money_tx.originator.clone(),
                    send_money_tx.receiver.clone(),
                    send_money_tx.amount,
                ));
            } else {
                unreachable!();
            }
        }

        let mut new_receipts = HashMap::new();

        for (from, to, amount) in balance_transfers {
            let mut good_to_go = false;

            if self.account_id_to_shard_id(&from) != shard_id {
                // This is a receipt, was already debited
                good_to_go = true;
            } else if let Some(balance) = accounts_mapping.get(&from) {
                if *balance >= amount {
                    let new_balance = balance - amount;
                    accounts_mapping.insert(from.clone(), new_balance);
                    good_to_go = true;
                }
            }

            if good_to_go {
                let new_receipt_hashes = if self.account_id_to_shard_id(&to) == shard_id {
                    accounts_mapping
                        .insert(to.clone(), accounts_mapping.get(&to).unwrap_or(&0) + amount);
                    vec![]
                } else {
                    info!(
                        "MOO creating receipt from shard {} from {} to {} amt {}",
                        shard_id, from, to, amount
                    );
                    let receipt = ReceiptTransaction::new(
                        from.clone(),
                        to,
                        CryptoHash::default(),
                        NewCall(AsyncCall::new(
                            vec![],
                            vec![],
                            amount,
                            AccountId::default(),
                            from,
                            PublicKey::empty(),
                        )),
                    );
                    let receipt_hash = receipt.get_hash();
                    new_receipts.entry(receipt.shard_id()).or_insert_with(|| vec![]).push(receipt);
                    vec![receipt_hash]
                };

                tx_results.push(TransactionResult {
                    status: TransactionStatus::Completed,
                    logs: vec![],
                    receipts: new_receipt_hashes,
                    result: None,
                });
            }
        }

        let mut new_balances = vec![];
        for validator in self.validators.iter() {
            let mut seen = false;
            for (key, value) in accounts_mapping.iter() {
                if key == &validator.account_id {
                    assert!(!seen);
                    seen = true;
                    new_balances.push(*value);
                }
            }
            if !seen {
                new_balances.push(0);
            }
        }
        let (new_state_root, _) = merklize(&new_balances);
        self.amounts.write().unwrap().insert(new_state_root, accounts_mapping);

        info!(
            "MOO Applied transactions in shard {}, new state root {}, balances {:?}",
            shard_id, new_state_root, new_balances
        );

        Ok((
            WrappedTrieChanges::new(self.trie.clone(), TrieChanges::empty(state_root.clone())),
            new_state_root,
            tx_results,
            new_receipts,
            vec![],
        ))
    }

    fn query(
        &self,
        state_root: MerkleHash,
        _height: BlockIndex,
        path: Vec<&str>,
        _data: &[u8],
    ) -> Result<QueryResponse, Box<dyn std::error::Error>> {
        let account_id = path[1].to_string();
        let account_id2 = account_id.clone();
        Ok(QueryResponse::ViewAccount(AccountViewCallResult {
            account_id,
            nonce: 0,
            amount: self
                .amounts
                .read()
                .unwrap()
                .get(&state_root)
                .map_or_else(|| 0, |mapping| *mapping.get(&account_id2).unwrap_or(&0)),
            stake: 0,
            public_keys: vec![],
            code_hash: CryptoHash::default(),
        }))
    }
}

pub fn setup() -> (Chain, Arc<KeyValueRuntime>, Arc<InMemorySigner>) {
    let store = create_test_store();
    let runtime = Arc::new(KeyValueRuntime::new(store.clone()));
    let chain = Chain::new(store, runtime.clone(), Utc::now()).unwrap();
    let signer = Arc::new(InMemorySigner::from_seed("test", "test"));
    (chain, runtime, signer)
}
