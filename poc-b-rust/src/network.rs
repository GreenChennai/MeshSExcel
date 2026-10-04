use anyhow::Result;
use rocksdb::{IteratorMode, DB};
use std::path::Path;

use crate::block::Block;

pub struct BlockStore {
    db: DB,
}

impl BlockStore {
    pub fn open(path: &Path) -> Result<Self> {
        let db = DB::open_default(path)?;
        Ok(Self { db })
    }

    pub fn put_block(&self, block: &Block) -> Result<()> {
        let key = format!("doc:{}:block:{}", block.header.doc_id, block.hash_key());
        let value = serde_json::to_vec(block)?;
        self.db.put(key.as_bytes(), value)?;
        Ok(())
    }

    pub fn list_blocks(&self, doc_id: &str) -> Result<Vec<Block>> {
        let prefix = format!("doc:{}:block:", doc_id);
        let iter = self.db.iterator(IteratorMode::From(prefix.as_bytes(), rocksdb::Direction::Forward));
        let mut blocks = Vec::new();

        for item in iter {
            let (key, value) = item?;
            let key_str = String::from_utf8_lossy(&key);

            if !key_str.starts_with(&prefix) {
                break;
            }

            let block: Block = serde_json::from_slice(&value)?;
            blocks.push(block);
        }

        Ok(blocks)
    }

    pub fn latest_block(&self, doc_id: &str) -> Result<Option<Block>> {
        let blocks = self.list_blocks(doc_id)?;
        Ok(blocks.into_iter().last())
    }

    pub fn count_blocks(&self, doc_id: &str) -> Result<usize> {
        Ok(self.list_blocks(doc_id)?.len())
    }

    pub fn contains(&self, doc_id: &str, hash: &str) -> Result<bool> {
        let key = format!("doc:{}:block:{}", doc_id, hash);
        Ok(self.db.get(key.as_bytes())?.is_some())
    }
}
