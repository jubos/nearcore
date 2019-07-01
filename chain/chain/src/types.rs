use std::collections::HashMap;

pub use near_primitives::block::{Block, BlockHeader, Weight};
use near_primitives::crypto::signature::Signature;
use near_primitives::crypto::signer::EDSigner;
use near_primitives::hash::CryptoHash;
use near_primitives::rpc::QueryResponse;
use near_primitives::transaction::{ReceiptTransaction, SignedTransaction, TransactionResult};
use near_primitives::types::{AccountId, BlockIndex, MerkleHash, ShardId, ValidatorStake};
use near_store::{StoreUpdate, WrappedTrieChanges};

use crate::error::Error;
use near_primitives::sharding::{ChunkOnePart, ShardChunk, ShardChunkHeader};

#[derive(Eq, PartialEq, Debug)]
pub enum BlockStatus {
    /// Block is the "next" block, updating the chain head.
    Next,
    /// Block does not update the chain head and is a fork.
    Fork,
    /// Block updates the chain head via a (potentially disruptive) "reorg".
    /// Previous block was not our previous chain head.
    Reorg(CryptoHash),
}

/// Options for block origin.
#[derive(Eq, PartialEq)]
pub enum Provenance {
    /// No provenance.
    NONE,
    /// Adds block while in syncing mode.
    SYNC,
    /// Block we produced ourselves.
    PRODUCED,
}

/// Information about valid transaction that was processed by chain + runtime.
pub struct ValidTransaction {
    pub transaction: SignedTransaction,
}

/// Map of shard to list of receipts to send to it.
pub type ReceiptResult = HashMap<ShardId, Vec<ReceiptTransaction>>;

pub enum ShardFullChunkOrOnePart<'a> {
    // The validator follows the shard, and has the full chunk
    FullChunk(&'a ShardChunk),
    // The validator doesn't follow the shard, and only has one part
    OnePart(&'a ChunkOnePart),
    // The chunk for particular shard is not present in the block
    NoChunk,
}

/// Bridge between the chain and the runtime.
/// Main function is to update state given transactions.
/// Additionally handles validators and block weight computation.
pub trait RuntimeAdapter: Send + Sync {
    /// Initialize state to genesis state and returns StoreUpdate, state root and initial validators.
    /// StoreUpdate can be discarded if the chain past the genesis.
    fn genesis_state(&self) -> (StoreUpdate, Vec<MerkleHash>);

    /// Verify block producer validity and return weight of given block for fork choice rule.
    fn compute_block_weight(
        &self,
        prev_header: &BlockHeader,
        header: &BlockHeader,
    ) -> Result<Weight, Error>;

    fn verify_chunk_header_signature(&self, header: &ShardChunkHeader) -> bool;

    /// Epoch block proposers with number of seats they have for given shard.
    /// Returns error if height is outside of known boundaries.
    fn get_epoch_block_proposers(
        &self,
        parent_hash: CryptoHash,
        height: BlockIndex,
    ) -> Result<Vec<(AccountId, u64)>, Box<dyn std::error::Error>>;

    /// Block proposer for given height for the main block. Return error if outside of known boundaries.
    fn get_block_proposer(
        &self,
        parent_hash: CryptoHash,
        height: BlockIndex,
    ) -> Result<AccountId, Box<dyn std::error::Error>>;

    /// Chunk proposer for given height for given shard. Return error if outside of known boundaries.
    fn get_chunk_proposer(
        &self,
        parent_hash: CryptoHash,
        height: BlockIndex,
        shard_id: ShardId,
    ) -> Result<AccountId, Box<dyn std::error::Error>>;

    /// Check validator's signature.
    fn check_validator_signature(&self, account_id: &AccountId, signature: &Signature) -> bool;

    /// Get current number of shards.
    fn num_shards(&self) -> ShardId;

    fn num_total_parts(&self, parent_hash: CryptoHash, height: BlockIndex) -> usize;
    fn num_data_parts(&self, parent_hash: CryptoHash, height: BlockIndex) -> usize;

    /// Account Id to Shard Id mapping, given current number of shards.
    fn account_id_to_shard_id(&self, account_id: &AccountId) -> ShardId;
    fn get_part_owner(
        &self,
        parent_hash: CryptoHash,
        height: BlockIndex,
        part_id: u64,
    ) -> Result<AccountId, Box<dyn std::error::Error>>;

    fn cares_about_shard(
        &self,
        account_id: &AccountId,
        parent_hash: CryptoHash,
        height: BlockIndex,
        shard_id: ShardId,
    ) -> bool;

    /// Validate transaction and return transaction information relevant to ordering it in the mempool.
    fn validate_tx(
        &self,
        shard_id: ShardId,
        state_root: MerkleHash,
        transaction: SignedTransaction,
    ) -> Result<ValidTransaction, String>;

    /// Add proposals for validators.
    fn add_validator_proposals(
        &self,
        parent_hash: CryptoHash,
        current_hash: CryptoHash,
        block_index: BlockIndex,
        proposals: Vec<ValidatorStake>,
    ) -> Result<(), Box<dyn std::error::Error>>;

    /// Apply transactions to given state root and return store update and new state root.
    /// Also returns transaction result for each transaction and new receipts.
    fn apply_transactions(
        &self,
        shard_id: ShardId,
        merkle_hash: &MerkleHash,
        block_index: BlockIndex,
        prev_block_hash: &CryptoHash,
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
    >;

    /// Query runtime with given `path` and `data`.
    fn query(
        &self,
        state_root: MerkleHash,
        height: BlockIndex,
        path_parts: Vec<&str>,
        data: &[u8],
    ) -> Result<QueryResponse, Box<dyn std::error::Error>>;
}

/// The tip of a fork. A handle to the fork ancestry from its leaf in the
/// blockchain tree. References the max height and the latest and previous
/// blocks for convenience and the total weight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tip {
    /// Height of the tip (max height of the fork)
    pub height: BlockIndex,
    /// Last block pushed to the fork
    pub last_block_hash: CryptoHash,
    /// Previous block
    pub prev_block_hash: CryptoHash,
    /// Total weight on that fork
    pub total_weight: Weight,
}

impl Tip {
    /// Creates a new tip based on provided header.
    pub fn from_header(header: &BlockHeader) -> Tip {
        Tip {
            height: header.height,
            last_block_hash: header.hash(),
            prev_block_hash: header.prev_hash,
            total_weight: header.total_weight,
        }
    }
}

/// Block approval by other block producers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockApproval {
    pub hash: CryptoHash,
    pub signature: Signature,
    pub target: AccountId,
}

impl BlockApproval {
    pub fn new(hash: CryptoHash, signer: &dyn EDSigner, target: AccountId) -> Self {
        let signature = signer.sign(hash.as_ref());
        BlockApproval { hash, signature, target }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;

    use near_primitives::crypto::signer::InMemorySigner;

    use super::*;

    #[test]
    fn test_block_produce() {
        let num_shards = 32;
        let genesis = Block::genesis(vec![MerkleHash::default()], Utc::now(), num_shards);
        let signer = Arc::new(InMemorySigner::from_seed("other", "other"));
        let b1 = Block::produce(
            &genesis.header,
            1,
            Block::genesis_chunks(vec![Block::genesis_hash()], num_shards),
            vec![],
            HashMap::default(),
            vec![],
            signer.clone(),
        );
        assert!(signer.verify(b1.hash().as_ref(), &b1.header.signature));
        assert_eq!(b1.header.total_weight.to_num(), 1);
        let other_signer = Arc::new(InMemorySigner::from_seed("other2", "other2"));
        let approvals: HashMap<usize, Signature> =
            vec![(1, other_signer.sign(b1.hash().as_ref()))].into_iter().collect();
        let b2 = Block::produce(&b1.header, 2, vec![], vec![], approvals, vec![], signer.clone());
        assert!(signer.verify(b2.hash().as_ref(), &b2.header.signature));
        assert_eq!(b2.header.total_weight.to_num(), 3);
    }
}
