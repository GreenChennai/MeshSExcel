//! 节点应用状态:REST 与 gossip 两个入口最终都汇到这里的几个方法。
//!
//! 写路径:`commit_ops`(本节点产生,签名+广播)与 `ingest`(远端 block,
//! 校验+吸收),两者共用同一套 LWW 应用与落库逻辑。

use anyhow::{Context, Result};
use chrono::Utc;
use ed25519_dalek::SigningKey;
use meshsexcel_core::store::{CellCacheRow, DocumentSummary, PeerRow};
use meshsexcel_core::{
    new_document_id, Block, CellOp, OpKind, OpStamp, Store, Workbook, DEFAULT_SHEET,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

/// block 吸收结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptOutcome {
    Applied,
    Duplicate,
    Rejected(String),
}

pub struct AppState {
    pub node_id: String,
    pub signing: SigningKey,
    pub store: Arc<Store>,
    pub db_dir: PathBuf,
    workbooks: Mutex<HashMap<String, Workbook>>,
    pub publish_tx: UnboundedSender<Vec<u8>>,
}

impl AppState {
    /// 打开存档、加载/生成节点身份、从 block 链重建工作簿内存态。
    pub fn boot(
        db_dir: &Path,
        node_id: &str,
        publish_tx: UnboundedSender<Vec<u8>>,
    ) -> Result<Self> {
        std::fs::create_dir_all(db_dir)?;
        let store = Store::open(&db_dir.join("meshsexcel.db")).context("open sqlite store")?;
        let signing = load_or_create_identity(&db_dir.join("identity.key"))?;

        let mut workbooks = HashMap::new();
        for doc in store.list_documents()? {
            let mut wb = Workbook::new(&doc.id);
            for block in store.list_blocks(&doc.id)? {
                match block.decoded_ops() {
                    Ok(ops) => {
                        wb.apply_ops(&ops);
                    }
                    Err(e) => {
                        eprintln!(
                            "[{node_id}] skip undecodable block {}: {e}",
                            block.hash_key()
                        )
                    }
                }
            }
            workbooks.insert(doc.id.clone(), wb);
        }
        eprintln!(
            "[{node_id}] booted: {} documents restored, identity {}",
            workbooks.len(),
            hex::encode(signing.verifying_key().to_bytes())
        );

        Ok(Self {
            node_id: node_id.to_string(),
            signing,
            store: Arc::new(store),
            db_dir: db_dir.to_path_buf(),
            workbooks: Mutex::new(workbooks),
            publish_tx,
        })
    }

    pub fn verifying_key_hex(&self) -> String {
        format!(
            "ed25519:{}",
            hex::encode(self.signing.verifying_key().to_bytes())
        )
    }

    // ------------------------------------------------------------------
    // 读路径
    // ------------------------------------------------------------------

    pub fn list_documents(&self) -> Result<Vec<DocumentSummary>> {
        self.store.list_documents().map_err(anyhow::Error::from)
    }

    pub fn get_document(&self, doc_id: &str) -> Result<Option<DocumentSummary>> {
        self.store.get_document(doc_id).map_err(anyhow::Error::from)
    }

    pub fn list_blocks(&self, doc_id: &str, since: Option<&str>) -> Result<Vec<Block>> {
        match since {
            None => self.store.list_blocks(doc_id).map_err(anyhow::Error::from),
            Some(s) => self
                .store
                .blocks_since(doc_id, s)
                .map_err(anyhow::Error::from),
        }
    }

    /// 本地全部 block(MeshReady 时全量重广播,补齐后加入的节点)。
    pub fn all_blocks(&self) -> Result<Vec<Block>> {
        let mut out = Vec::new();
        for doc in self.store.list_documents()? {
            out.extend(self.store.list_blocks(&doc.id)?);
        }
        Ok(out)
    }

    pub fn list_peers(&self) -> Result<Vec<PeerRow>> {
        self.store.list_peers().map_err(anyhow::Error::from)
    }

    /// 整簿计算后的网格(UI 一次拉全量;规模可控)。
    pub fn document_grid(
        &self,
        doc_id: &str,
    ) -> Result<Option<meshsexcel_core::sheet::WorkbookGrid>> {
        let wbs = self.workbooks.lock().unwrap();
        let Some(wb) = wbs.get(doc_id) else {
            return Ok(None);
        };
        Ok(Some(wb.grid()))
    }

    /// 对工作簿跑只读闭包(导出/快照等)。
    pub fn with_workbook<R>(
        &self,
        doc_id: &str,
        f: impl FnOnce(&meshsexcel_core::Workbook) -> R,
    ) -> Option<R> {
        let wbs = self.workbooks.lock().unwrap();
        wbs.get(doc_id).map(f)
    }

    // ------------------------------------------------------------------
    // 写路径
    // ------------------------------------------------------------------

    /// 新建文档:CreateDocument + 默认表,装进首个 block 广播。
    pub fn create_document(&self, name: &str, owner: &str) -> Result<DocumentSummary> {
        let id = new_document_id();
        let now = Utc::now().timestamp_millis();
        let ops = vec![
            CellOp::new(
                now,
                &self.node_id,
                OpKind::CreateDocument {
                    name: name.to_string(),
                    owner: owner.to_string(),
                },
            ),
            CellOp::new(
                now,
                &self.node_id,
                OpKind::AddSheet {
                    sheet: DEFAULT_SHEET.to_string(),
                    position: None,
                },
            ),
        ];
        let summary = DocumentSummary {
            id: id.clone(),
            name: name.to_string(),
            owner: Some(owner.to_string()),
            head_block: None,
            created_at: None,
        };
        self.store.insert_document(&summary)?;
        self.commit_ops(&id, ops)?;
        Ok(summary)
    }

    /// UI / REST 的本地编辑入口:补全 stamp 后签发 block。
    /// `ts == 0` 的 op 由服务端统一盖当前时间。
    pub fn apply_local_ops(&self, doc_id: &str, mut ops: Vec<CellOp>) -> Result<Block> {
        if self.store.get_document(doc_id)?.is_none() {
            anyhow::bail!("document {doc_id} not found");
        }
        let now = Utc::now().timestamp_millis();
        for op in &mut ops {
            if op.stamp.ts == 0 {
                op.stamp = OpStamp::new(now, self.node_id.clone());
            }
        }
        self.commit_ops(doc_id, ops)
    }

    /// 以本节点身份签发并吸收一个 block。
    pub fn commit_ops(&self, doc_id: &str, ops: Vec<CellOp>) -> Result<Block> {
        let prev = self.current_head(doc_id)?;
        let block = Block::from_ops(doc_id, prev, &self.node_id, &self.signing, &ops);
        self.ingest(block.clone(), true)?;
        Ok(block)
    }

    /// 校验并吸收一个 block(本地或远端);`broadcast` 决定是否再上 gossip。
    pub fn ingest(&self, block: Block, broadcast: bool) -> Result<AcceptOutcome> {
        if !block.verify() {
            return Ok(AcceptOutcome::Rejected(
                "signature/merkle verification failed".into(),
            ));
        }
        let doc_id = block.header.doc_id.clone();
        let ops = block.decoded_ops()?;
        // 远端首见文档:先物化文档行(blocks 表对 documents 有外键)。
        // gossip 不保证块序:CreateDocument 块可能后到,见到它就补正文档名
        if let Some(create) = ops.iter().find_map(|op| match &op.kind {
            OpKind::CreateDocument { name, owner } => Some((name.clone(), owner.clone())),
            _ => None,
        }) {
            if self.store.get_document(&doc_id)?.is_none() {
                self.store.insert_document(&DocumentSummary {
                    id: doc_id.clone(),
                    name: create.0.clone(),
                    owner: Some(create.1.clone()),
                    head_block: None,
                    created_at: None,
                })?;
            } else {
                self.store
                    .update_document_meta(&doc_id, &create.0, &create.1)?;
            }
        } else if self.store.get_document(&doc_id)?.is_none() {
            self.store.insert_document(&DocumentSummary {
                id: doc_id.clone(),
                name: doc_id.clone(),
                owner: None,
                head_block: None,
                created_at: None,
            })?;
        }
        if !self.store.put_block(&block)? {
            return Ok(AcceptOutcome::Duplicate);
        }
        // 新 block:应用 ops、同步缓存与链头
        let sheet_names = {
            let mut wbs = self.workbooks.lock().unwrap();
            let wb = wbs
                .entry(doc_id.clone())
                .or_insert_with(|| Workbook::new(&doc_id));
            wb.apply_ops(&ops);
            wb.sheet_names()
        };
        for (pos, s) in sheet_names.iter().enumerate() {
            self.store.upsert_sheet(&doc_id, s, pos)?;
        }
        let wbs = self.workbooks.lock().unwrap();
        let wb = wbs.get(&doc_id).expect("entry above");
        self.sync_cells_cache(&doc_id, wb)?;
        drop(wbs);
        self.store
            .set_head(&doc_id, self.current_head(&doc_id)?.as_deref())?;
        if broadcast {
            let _ = self
                .publish_tx
                .send(serde_json::to_vec(&block).unwrap_or_default());
        }
        Ok(AcceptOutcome::Applied)
    }

    fn current_head(&self, doc_id: &str) -> Result<Option<String>> {
        // list_blocks 按 (timestamp, hash) 升序;链头取最大者
        let blocks = self.store.list_blocks(doc_id)?;
        Ok(blocks.last().map(|b| b.hash_key()))
    }

    fn sync_cells_cache(&self, doc_id: &str, wb: &Workbook) -> Result<()> {
        let mut rows = Vec::new();
        for (key, rec) in wb.all_cells() {
            if meshsexcel_core::sheet::is_tombstone(rec) {
                continue; // 墓碑不入缓存
            }
            rows.push(CellCacheRow {
                doc_id: doc_id.to_string(),
                sheet: key.0.clone(),
                row: key.1 as i64,
                col: key.2 as i64,
                value: rec.value.clone(),
                formula: rec.formula.clone(),
                style: rec
                    .style
                    .as_ref()
                    .map(|s| serde_json::to_string(s).unwrap_or_default()),
            });
        }
        self.store
            .replace_cells(doc_id, &rows)
            .map_err(anyhow::Error::from)
    }

    // ------------------------------------------------------------------
    // 快照
    // ------------------------------------------------------------------

    pub fn create_snapshot(&self, doc_id: &str, description: Option<&str>) -> Result<String> {
        let wbs = self.workbooks.lock().unwrap();
        let wb = wbs
            .get(doc_id)
            .ok_or_else(|| anyhow::anyhow!("document {doc_id} not found"))?;
        let blob = serde_json::to_vec(&wb.snapshot())?;
        drop(wbs);
        let id = format!("snap-{}", uuid::Uuid::new_v4().simple());
        self.store
            .create_snapshot(&id, doc_id, &blob, description)?;
        Ok(id)
    }

    pub fn list_snapshots(
        &self,
        doc_id: &str,
    ) -> Result<Vec<meshsexcel_core::store::SnapshotMeta>> {
        self.store
            .list_snapshots(doc_id)
            .map_err(anyhow::Error::from)
    }

    /// 恢复 = 把快照内容作为新的 LWW 变更重新提交;快照里没有、
    /// 当前存在的格子会被清空,保证回到快照时点。
    pub fn restore_snapshot(&self, doc_id: &str, snapshot_id: &str) -> Result<usize> {
        let blob = self
            .store
            .get_snapshot(snapshot_id)?
            .ok_or_else(|| anyhow::anyhow!("snapshot {snapshot_id} not found"))?;
        let sheets: Vec<meshsexcel_core::sheet::SheetSnapshot> = serde_json::from_slice(&blob)?;
        let now = Utc::now().timestamp_millis();

        let mut ops = Vec::new();
        let mut snapshot_keys: std::collections::HashSet<(String, u32, u32)> =
            std::collections::HashSet::new();
        for sheet in &sheets {
            ops.push(CellOp::new(
                now,
                &self.node_id,
                OpKind::AddSheet {
                    sheet: sheet.name.clone(),
                    position: None,
                },
            ));
            for cell in &sheet.cells {
                snapshot_keys.insert((sheet.name.clone(), cell.row, cell.col));
                ops.push(CellOp::new(
                    now,
                    &self.node_id,
                    OpKind::SetCell {
                        sheet: sheet.name.clone(),
                        row: cell.row,
                        col: cell.col,
                        value: cell.value.clone(),
                        formula: cell.formula.clone(),
                        style: cell.style.clone(),
                    },
                ));
            }
        }
        // 清掉快照之后新增的格子
        let stale: Vec<(String, u32, u32)> = {
            let wbs = self.workbooks.lock().unwrap();
            match wbs.get(doc_id) {
                Some(wb) => wb
                    .all_cells()
                    .map(|(k, _)| k.clone())
                    .filter(|k| !snapshot_keys.contains(k))
                    .collect(),
                None => Vec::new(),
            }
        };
        for (sheet, row, col) in stale {
            ops.push(CellOp::new(
                now,
                &self.node_id,
                OpKind::SetCell {
                    sheet,
                    row,
                    col,
                    value: None,
                    formula: None,
                    style: None,
                },
            ));
        }
        let count = ops.len();
        self.apply_local_ops(doc_id, ops)?;
        Ok(count)
    }
}

/// 节点身份持久化:32 字节种子存 `identity.key`(hex),保证重启后
/// 审计链上的作者身份稳定。
fn load_or_create_identity(path: &Path) -> Result<SigningKey> {
    if path.exists() {
        let text = std::fs::read_to_string(path).context("read identity.key")?;
        let raw = hex::decode(text.trim()).context("decode identity.key")?;
        let bytes: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("identity key must be 32 bytes"))?;
        return Ok(SigningKey::from_bytes(&bytes));
    }
    let key = SigningKey::generate(&mut rand::rngs::OsRng);
    std::fs::write(path, hex::encode(key.to_bytes()))?;
    Ok(key)
}
