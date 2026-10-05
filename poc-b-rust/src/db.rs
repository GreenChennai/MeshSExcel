//! block 持久化:SQLite(`spec/sqlite_schema.sql` 的 blocks 表;
//! 完整 block 以 JSON 存于 payload 列,其余列供 SQL 层检索)。

use crate::block::Block;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS blocks (
  block_hash TEXT PRIMARY KEY,
  doc_id TEXT NOT NULL,
  prev_hash TEXT,
  author TEXT,
  author_pubkey TEXT,
  timestamp DATETIME,
  payload BLOB,
  merkle_root TEXT,
  signature BLOB
);
CREATE INDEX IF NOT EXISTS idx_blocks_doc ON blocks(doc_id, timestamp);
";

pub struct BlockStore {
    conn: Connection,
}

impl BlockStore {
    pub fn open(db_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(db_dir)?;
        let conn = Connection::open(db_dir.join("blocks.db"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// 写入 block;重复 hash 静默忽略(gossip 天然重复)。
    pub fn put_block(&self, block: &Block) -> Result<()> {
        let json = serde_json::to_vec(block)?;
        self.conn.execute(
            "INSERT OR IGNORE INTO blocks
             (block_hash, doc_id, prev_hash, author, author_pubkey, timestamp, payload, merkle_root, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                block.hash_key(),
                block.header.doc_id,
                block.header.prev_hash,
                block.header.author,
                block.header.author_pubkey,
                block.header.timestamp.to_rfc3339(),
                json,
                block.merkle_root,
                block.signature,
            ],
        )?;
        Ok(())
    }

    pub fn contains(&self, doc_id: &str, hash: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM blocks WHERE doc_id = ?1 AND block_hash = ?2",
            params![doc_id, hash],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn count_blocks(&self, doc_id: &str) -> Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM blocks WHERE doc_id = ?1",
            params![doc_id],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// 文档链头(重启后据此续链)。
    pub fn latest_block(&self, doc_id: &str) -> Result<Option<Block>> {
        let payload: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT payload FROM blocks WHERE doc_id = ?1
                 ORDER BY timestamp DESC, block_hash DESC LIMIT 1",
                params![doc_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(payload.as_deref().map(serde_json::from_slice).transpose()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use meshsexcel_core::ops::{CellOp, OpKind};

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("meshsexcel-pocb-{tag}-{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample(author: &str, ts_note: &str) -> Block {
        let key = SigningKey::from_bytes(&[11u8; 32]);
        let op = CellOp::new(
            1,
            author,
            OpKind::SetCell {
                sheet: "sheet-demo".into(),
                row: 1,
                col: 1,
                value: Some(ts_note.into()),
                formula: None,
                style: None,
            },
        );
        Block::from_ops("sheet-demo", None, author, &key, &[op])
    }

    #[test]
    fn put_latest_dedup() {
        let dir = tmp_dir("put");
        let store = BlockStore::open(&dir).unwrap();
        assert_eq!(store.count_blocks("sheet-demo").unwrap(), 0);
        assert!(store.latest_block("sheet-demo").unwrap().is_none());

        let b1 = sample("alice", "first");
        let b2 = sample("alice", "second");
        store.put_block(&b1).unwrap();
        store.put_block(&b2).unwrap();
        store.put_block(&b1).unwrap(); // 重复

        assert_eq!(store.count_blocks("sheet-demo").unwrap(), 2);
        let latest = store.latest_block("sheet-demo").unwrap().unwrap();
        assert!(latest.verify());
        assert!(store.contains("sheet-demo", &b1.hash_key()).unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }
}
