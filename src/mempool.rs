use bitcoin::network::message_blockdata::Inventory;
use bitcoin::{Transaction, Txid, Wtxid};
use std::collections::HashMap;
use std::sync::RwLock;
use anyhow::anyhow;

/// Container for unconfirmed, but valid Bitcoin transactions
///
/// It is normally shared between [`Peer`]s with the use of [`Arc`], so that transactions are not
/// duplicated in memory.
#[derive(Debug, Default)]
pub struct Mempool(RwLock<InnerMempool>);

#[derive(Debug, Default)]
struct InnerMempool {
    txs: HashMap<Txid, Transaction>,
    wtxids: HashMap<Wtxid, Txid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TxIdentifier {
    Wtxid(Wtxid),
    Txid(Txid),
}

impl Mempool {
    /// Create a new empty mempool
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a transaction to the mempool
    ///
    /// Note that this doesn't propagate the transaction to other
    /// peers. To do that, [`broadcast`](crate::blockchain::Blockchain::broadcast) should be used.
    pub fn add_tx(&self, tx: Transaction) -> anyhow::Result<()> {
        let mut guard = self.0.write().map_err(|_| anyhow!("Failed to acquire write lock"))?;

        guard.wtxids.insert(tx.wtxid(), tx.txid());
        guard.txs.insert(tx.txid(), tx);

        Ok(())
    }

    /// Look-up a transaction in the mempool given an [`Inventory`] request
    pub fn get_tx(&self, inventory: &Inventory) -> anyhow::Result<Option<Transaction>> {
        let identifier = match inventory {
            Inventory::Error
            | Inventory::Block(_)
            | Inventory::WitnessBlock(_)
            | Inventory::CompactBlock(_) => return Ok(None),
            Inventory::Transaction(txid) => TxIdentifier::Txid(*txid),
            Inventory::WitnessTransaction(txid) => TxIdentifier::Txid(*txid),
            Inventory::WTx(wtxid) => TxIdentifier::Wtxid(*wtxid),
            Inventory::Unknown { inv_type, hash } => {
                println!(
                    "Unknown inventory request type `{inv_type}`, hash `{hash:?}`",
                );
                return Ok(None);
            }
        };

        let txid = match identifier {
            TxIdentifier::Txid(txid) => Some(txid),
            TxIdentifier::Wtxid(wtxid) => self.0.read().map_err(|_| anyhow!("Failed to acquire read lock"))?.wtxids.get(&wtxid).cloned(),
        };

        match txid {
            None => Ok(None),
            Some(txid) => Ok(self.0.read().map_err(|_| anyhow!("Failed to acquire read lock"))?.txs.get(&txid).cloned())
        }
    }

    /// Return whether or not the mempool contains a transaction with a given txid
    pub fn has_tx(&self, txid: &Txid) -> anyhow::Result<bool> {
        Ok(self.0.read().map_err(|_| anyhow!("Failed to read write lock"))?.txs.contains_key(txid))
    }

    /// Return the list of transactions contained in the mempool
    pub fn iter_txs(&self) -> anyhow::Result<Vec<Transaction>> {
        Ok(self.0.read().map_err(|_| anyhow!("Failed to read write lock"))?.txs.values().cloned().collect())
    }
}
