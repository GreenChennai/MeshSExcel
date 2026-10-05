//! MeshSExcel 核心库。
//!
//! - [`block`] — 区块链式审计(block 生成 / 签名 / 校验)
//! - [`ops`] — 单元格级 LWW(Last-Writer-Wins)操作,保证多节点乱序合并收敛
//! - [`sheet`] — 工作簿内存状态、依赖图增量公式重算
//! - [`formula`] — Excel 风格公式解析与求值
//! - [`store`] — SQLite 本地存档(遵循 `spec/sqlite_schema.sql`)
//! - [`xlsx`] — XLSX / CSV 导入导出

pub mod block;
pub mod error;
pub mod formula;
pub mod ops;
pub mod sheet;
pub mod store;
pub mod xlsx;

pub use block::{Block, BlockHeader, BlockPayload};
pub use error::{Error, Result};
pub use formula::Value;
pub use ops::{CellOp, CellStyle, OpKind, OpStamp};
pub use sheet::{CellRecord, Workbook};
pub use store::Store;

/// 默认工作表名。
pub const DEFAULT_SHEET: &str = "Sheet1";

/// 为新文档生成 id(doc-<uuid>)。
pub fn new_document_id() -> String {
    format!("doc-{}", uuid::Uuid::new_v4().simple())
}
