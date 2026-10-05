//! block 模型与签名校验:实现统一放在 `meshsexcel-core`,这里薄再导出,
//! 保证 PoC 与正式节点使用完全一致的 block 语义。

// PoC 是二进制 crate,再导出未被 main 直接用到时会有 unused 告警
#[allow(unused_imports)]
pub use meshsexcel_core::block::{Block, BlockHeader, BlockPayload};
