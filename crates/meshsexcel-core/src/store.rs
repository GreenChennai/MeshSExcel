//! SQLite 本地存档,建表遵循 `spec/sqlite_schema.sql`。
//!
//! 唯一 schema 补充:`sheets` 表(简化 schema 中表名只隐含在 cells_cache 里,
//! MVP 需要稳定的表序,故显式建表)。

use crate::block::Block;
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

/// 文档摘要(对应 openapi DocumentSummary)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSummary {
    pub id: String,
    pub name: String,
    pub owner: Option<String>,
    pub head_block: Option<String>,
    pub created_at: Option<String>,
}

/// 工作表行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SheetRow {
    pub doc_id: String,
    pub name: String,
    pub position: i64,
}

/// cells_cache 行(字面量层,不含公式计算结果)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellCacheRow {
    pub doc_id: String,
    pub sheet: String,
    pub row: i64,
    pub col: i64,
    pub value: Option<String>,
    pub formula: Option<String>,
    pub style: Option<String>,
}

/// 快照元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub snapshot_id: String,
    pub doc_id: String,
    pub created_at: Option<String>,
    pub description: Option<String>,
    pub size: i64,
}

/// peer 行(对应 openapi Peer)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRow {
    pub peer_id: String,
    pub addr: Option<String>,
    pub pubkey: Option<String>,
    pub last_seen: Option<String>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS documents (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  owner TEXT,
  head_block TEXT,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS blocks (
  block_hash TEXT PRIMARY KEY,
  doc_id TEXT NOT NULL,
  prev_hash TEXT,
  author TEXT,
  author_pubkey TEXT,
  timestamp DATETIME,
  payload BLOB,
  merkle_root TEXT,
  signature BLOB,
  FOREIGN KEY(doc_id) REFERENCES documents(id)
);

CREATE TABLE IF NOT EXISTS snapshots (
  snapshot_id TEXT PRIMARY KEY,
  doc_id TEXT NOT NULL,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  snapshot_blob BLOB,
  description TEXT
);

CREATE TABLE IF NOT EXISTS peers (
  peer_id TEXT PRIMARY KEY,
  addr TEXT,
  pubkey TEXT,
  last_seen DATETIME
);

CREATE TABLE IF NOT EXISTS cells_cache (
  doc_id TEXT,
  sheet TEXT,
  row INTEGER,
  col INTEGER,
  value TEXT,
  formula TEXT,
  style TEXT,
  PRIMARY KEY(doc_id, sheet, row, col)
);

CREATE TABLE IF NOT EXISTS sheets (
  doc_id TEXT,
  name TEXT,
  position INTEGER,
  PRIMARY KEY(doc_id, name)
);

CREATE INDEX IF NOT EXISTS idx_blocks_doc ON blocks(doc_id, timestamp);
"#;

/// SQLite 存档。内部一把锁:局域网 MVP 规模足够,且天然串行化。
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// 打开(或创建)存档。`":memory:"` 用于测试。
    pub fn open(path: &Path) -> Result<Self> {
        if path != Path::new(":memory:") {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::Crypto("store lock poisoned".into()))?;
        f(&conn)
    }

    // ------------------------------------------------------------------
    // documents / sheets
    // ------------------------------------------------------------------

    pub fn insert_document(&self, doc: &DocumentSummary) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO documents (id, name, owner, head_block) VALUES (?1, ?2, ?3, ?4)",
                params![doc.id, doc.name, doc.owner, doc.head_block],
            )?;
            Ok(())
        })
    }

    pub fn set_head(&self, doc_id: &str, head: Option<&str>) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE documents SET head_block = ?2 WHERE id = ?1",
                params![doc_id, head],
            )?;
            Ok(())
        })
    }

    pub fn list_documents(&self) -> Result<Vec<DocumentSummary>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, owner, head_block, created_at FROM documents ORDER BY created_at, id",
            )?;
            let rows = stmt
                .query_map([], row_document)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn get_document(&self, doc_id: &str) -> Result<Option<DocumentSummary>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, owner, head_block, created_at FROM documents WHERE id = ?1",
            )?;
            Ok(stmt.query_row(params![doc_id], row_document).optional()?)
        })
    }

    pub fn upsert_sheet(&self, doc_id: &str, name: &str, position: usize) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO sheets (doc_id, name, position) VALUES (?1, ?2, ?3)",
                params![doc_id, name, position as i64],
            )?;
            Ok(())
        })
    }

    /// 文档名/负责人更新。gossip 不保证 block 顺序:CreateDocument 块可能
    /// 晚于其他块到达,见到它就要把占位名(= id)补正。
    pub fn update_document_meta(&self, doc_id: &str, name: &str, owner: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE documents SET name = ?2, owner = ?3 WHERE id = ?1",
                params![doc_id, name, owner],
            )?;
            Ok(())
        })
    }

    pub fn remove_sheet(&self, doc_id: &str, name: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "DELETE FROM sheets WHERE doc_id = ?1 AND name = ?2",
                params![doc_id, name],
            )?;
            Ok(())
        })
    }

    pub fn list_sheets(&self, doc_id: &str) -> Result<Vec<String>> {
        self.with(|c| {
            let mut stmt =
                c.prepare("SELECT name FROM sheets WHERE doc_id = ?1 ORDER BY position, name")?;
            let rows = stmt
                .query_map(params![doc_id], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    // ------------------------------------------------------------------
    // blocks
    // ------------------------------------------------------------------

    /// 写入 block;返回 false 表示重复(hash 已存在),天然去重。
    pub fn put_block(&self, block: &Block) -> Result<bool> {
        let json = serde_json::to_vec(block)?;
        let hash = block.hash_key();
        self.with(|c| {
            let n = c.execute(
                "INSERT OR IGNORE INTO blocks
                 (block_hash, doc_id, prev_hash, author, author_pubkey, timestamp, payload, merkle_root, signature)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    hash,
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
            Ok(n > 0)
        })
    }

    pub fn contains_block(&self, doc_id: &str, hash: &str) -> Result<bool> {
        self.with(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM blocks WHERE doc_id = ?1 AND block_hash = ?2",
                params![doc_id, hash],
                |r| r.get(0),
            )?;
            Ok(n > 0)
        })
    }

    pub fn count_blocks(&self, doc_id: &str) -> Result<i64> {
        self.with(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM blocks WHERE doc_id = ?1",
                params![doc_id],
                |r| r.get(0),
            )?;
            Ok(n)
        })
    }

    /// 某文档全部 block,按 (timestamp, hash) 稳定排序(重放语义依赖它)。
    pub fn list_blocks(&self, doc_id: &str) -> Result<Vec<Block>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT payload FROM blocks WHERE doc_id = ?1 ORDER BY timestamp, block_hash",
            )?;
            let rows = stmt
                .query_map(params![doc_id], |r| r.get::<_, Vec<u8>>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.iter()
                .map(|b| Ok(serde_json::from_slice(b)?))
                .collect()
        })
    }

    /// `since` 的祖先链(含 since 本身)之外的全部 block。
    /// 请求方以此取"自己还缺的 block"。since 未知时返回全部。
    pub fn blocks_since(&self, doc_id: &str, since: &str) -> Result<Vec<Block>> {
        let all = self.list_blocks(doc_id)?;
        let by_hash: std::collections::HashMap<String, &Block> =
            all.iter().map(|b| (b.hash_key(), b)).collect();
        let mut ancestry: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut cur = Some(since.to_string());
        while let Some(h) = cur {
            if !ancestry.insert(h.clone()) {
                break; // 环防护
            }
            cur = by_hash.get(&h).and_then(|b| b.header.prev_hash.clone());
        }
        Ok(all
            .into_iter()
            .filter(|b| !ancestry.contains(&b.hash_key()))
            .collect())
    }

    // ------------------------------------------------------------------
    // cells_cache(字面量层落库,真源仍是 blocks)
    // ------------------------------------------------------------------

    pub fn replace_cells(&self, doc_id: &str, rows: &[CellCacheRow]) -> Result<()> {
        self.with(|c| {
            c.execute("DELETE FROM cells_cache WHERE doc_id = ?1", params![doc_id])?;
            for r in rows {
                c.execute(
                    "INSERT OR REPLACE INTO cells_cache
                     (doc_id, sheet, row, col, value, formula, style)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![r.doc_id, r.sheet, r.row, r.col, r.value, r.formula, r.style],
                )?;
            }
            Ok(())
        })
    }

    pub fn load_cells(&self, doc_id: &str) -> Result<Vec<CellCacheRow>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT doc_id, sheet, row, col, value, formula, style
                 FROM cells_cache WHERE doc_id = ?1 ORDER BY sheet, row, col",
            )?;
            let rows = stmt
                .query_map(params![doc_id], |r| {
                    Ok(CellCacheRow {
                        doc_id: r.get(0)?,
                        sheet: r.get(1)?,
                        row: r.get(2)?,
                        col: r.get(3)?,
                        value: r.get(4)?,
                        formula: r.get(5)?,
                        style: r.get(6)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    // ------------------------------------------------------------------
    // snapshots
    // ------------------------------------------------------------------

    pub fn create_snapshot(
        &self,
        snapshot_id: &str,
        doc_id: &str,
        blob: &[u8],
        description: Option<&str>,
    ) -> Result<SnapshotMeta> {
        self.with(|c| {
            c.execute(
                "INSERT INTO snapshots (snapshot_id, doc_id, snapshot_blob, description)
                 VALUES (?1, ?2, ?3, ?4)",
                params![snapshot_id, doc_id, blob, description],
            )?;
            Ok(SnapshotMeta {
                snapshot_id: snapshot_id.to_string(),
                doc_id: doc_id.to_string(),
                created_at: None,
                description: description.map(|s| s.to_string()),
                size: blob.len() as i64,
            })
        })
    }

    pub fn list_snapshots(&self, doc_id: &str) -> Result<Vec<SnapshotMeta>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT snapshot_id, doc_id, created_at, description, LENGTH(snapshot_blob)
                 FROM snapshots WHERE doc_id = ?1 ORDER BY created_at, snapshot_id",
            )?;
            let rows = stmt
                .query_map(params![doc_id], |r| {
                    Ok(SnapshotMeta {
                        snapshot_id: r.get(0)?,
                        doc_id: r.get(1)?,
                        created_at: r.get(2)?,
                        description: r.get(3)?,
                        size: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn get_snapshot(&self, snapshot_id: &str) -> Result<Option<Vec<u8>>> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT snapshot_blob FROM snapshots WHERE snapshot_id = ?1",
                params![snapshot_id],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten())
        })
    }

    // ------------------------------------------------------------------
    // peers
    // ------------------------------------------------------------------

    pub fn upsert_peer(
        &self,
        peer_id: &str,
        addr: Option<&str>,
        pubkey: Option<&str>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO peers (peer_id, addr, pubkey, last_seen)
                 VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)
                 ON CONFLICT(peer_id) DO UPDATE SET
                   addr = COALESCE(excluded.addr, peers.addr),
                   pubkey = COALESCE(excluded.pubkey, peers.pubkey),
                   last_seen = CURRENT_TIMESTAMP",
                params![peer_id, addr, pubkey],
            )?;
            Ok(())
        })
    }

    pub fn list_peers(&self) -> Result<Vec<PeerRow>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT peer_id, addr, pubkey, last_seen FROM peers ORDER BY last_seen DESC",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(PeerRow {
                        peer_id: r.get(0)?,
                        addr: r.get(1)?,
                        pubkey: r.get(2)?,
                        last_seen: r.get(3)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }
}

fn row_document(r: &Row<'_>) -> rusqlite::Result<DocumentSummary> {
    Ok(DocumentSummary {
        id: r.get(0)?,
        name: r.get(1)?,
        owner: r.get(2)?,
        head_block: r.get(3)?,
        created_at: r.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::CellOp;
    use ed25519_dalek::SigningKey;

    fn block(
        doc: &str,
        prev: Option<String>,
        author: &str,
        key: &SigningKey,
        ops: Vec<CellOp>,
    ) -> Block {
        Block::from_ops(doc, prev, author, key, &ops)
    }

    fn cell_op(ts: i64, author: &str, a1_val: &str) -> CellOp {
        CellOp::new(
            ts,
            author,
            crate::ops::OpKind::SetCell {
                sheet: "Sheet1".into(),
                row: 1,
                col: 1,
                value: Some(a1_val.into()),
                formula: None,
                style: None,
            },
        )
    }

    #[test]
    fn document_and_sheet_crud() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        assert!(store.list_documents().unwrap().is_empty());

        store
            .insert_document(&DocumentSummary {
                id: "doc-1".into(),
                name: "预算".into(),
                owner: Some("alice".into()),
                head_block: None,
                created_at: None,
            })
            .unwrap();
        let doc = store.get_document("doc-1").unwrap().unwrap();
        assert_eq!(doc.name, "预算");
        assert!(store.get_document("doc-x").unwrap().is_none());

        store.upsert_sheet("doc-1", "Sheet1", 0).unwrap();
        store.upsert_sheet("doc-1", "Data", 1).unwrap();
        assert_eq!(store.list_sheets("doc-1").unwrap(), vec!["Sheet1", "Data"]);
        store.remove_sheet("doc-1", "Sheet1").unwrap();
        assert_eq!(store.list_sheets("doc-1").unwrap(), vec!["Data"]);

        store.set_head("doc-1", Some("hash-9")).unwrap();
        assert_eq!(
            store.get_document("doc-1").unwrap().unwrap().head_block,
            Some("hash-9".into())
        );
    }

    #[test]
    fn blocks_dedup_and_ordering() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let key = SigningKey::from_bytes(&[9u8; 32]);
        store
            .insert_document(&DocumentSummary {
                id: "doc-1".into(),
                name: "d".into(),
                owner: None,
                head_block: None,
                created_at: None,
            })
            .unwrap();

        let b1 = block("doc-1", None, "alice", &key, vec![cell_op(1, "alice", "a")]);
        let h1 = b1.hash_key();
        assert!(store.put_block(&b1).unwrap());
        assert!(!store.put_block(&b1).unwrap(), "重复 hash 不重复入库");
        assert!(store.contains_block("doc-1", &h1).unwrap());
        assert_eq!(store.count_blocks("doc-1").unwrap(), 1);

        let b2 = block(
            "doc-1",
            Some(h1.clone()),
            "alice",
            &key,
            vec![cell_op(2, "alice", "b")],
        );
        let h2 = b2.hash_key();
        store.put_block(&b2).unwrap();

        let listed = store.list_blocks("doc-1").unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].hash_key(), h1, "按 (timestamp,hash) 稳定排序");

        let since_h1 = store.blocks_since("doc-1", &h1).unwrap();
        assert_eq!(since_h1.len(), 1);
        assert_eq!(since_h1[0].hash_key(), h2, "since 之后只给缺的");
        assert_eq!(store.blocks_since("doc-1", "unknown").unwrap().len(), 2);
    }

    #[test]
    fn cells_and_snapshots_and_peers() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        store
            .replace_cells(
                "doc-1",
                &[CellCacheRow {
                    doc_id: "doc-1".into(),
                    sheet: "Sheet1".into(),
                    row: 1,
                    col: 1,
                    value: Some("v".into()),
                    formula: None,
                    style: None,
                }],
            )
            .unwrap();
        let cells = store.load_cells("doc-1").unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].value.as_deref(), Some("v"));

        let meta = store
            .create_snapshot("snap-1", "doc-1", b"{}", Some("first"))
            .unwrap();
        assert_eq!(meta.size, 2);
        assert_eq!(store.list_snapshots("doc-1").unwrap().len(), 1);
        assert_eq!(store.get_snapshot("snap-1").unwrap(), Some(b"{}".to_vec()));

        store
            .upsert_peer("peer-a", Some("/ip4/1.2.3.4/tcp/1"), None)
            .unwrap();
        store
            .upsert_peer("peer-a", None, Some("ed25519:aa"))
            .unwrap();
        let peers = store.list_peers().unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(
            peers[0].addr.as_deref(),
            Some("/ip4/1.2.3.4/tcp/1"),
            "COALESCE 保留旧 addr"
        );
        assert_eq!(peers[0].pubkey.as_deref(), Some("ed25519:aa"));
    }

    #[test]
    fn block_roundtrip_preserves_fields() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let b = block("doc-9", None, "alice", &key, vec![cell_op(1, "alice", "x")]);
        // blocks 表对 documents 有外键(spec schema),先建文档
        store
            .insert_document(&DocumentSummary {
                id: "doc-9".into(),
                name: "d9".into(),
                owner: None,
                head_block: None,
                created_at: None,
            })
            .unwrap();
        store.put_block(&b).unwrap();
        let loaded = store.list_blocks("doc-9").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].header.author, "alice");
        assert_eq!(loaded[0].header.author_pubkey, b.header.author_pubkey);
        assert_eq!(loaded[0].payload.crdt_update_b64, b.payload.crdt_update_b64);
        assert_eq!(loaded[0].merkle_root, b.merkle_root);
        assert!(loaded[0].verify());
    }
}
