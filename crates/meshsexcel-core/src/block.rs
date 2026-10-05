use crate::ops::CellOp;
use crate::{Error, Result};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// block 头(字段与 `spec/block.proto` 的 BlockHeader 对应)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    pub doc_id: String,
    pub prev_hash: Option<String>,
    pub author: String,
    pub author_pubkey: String,
    pub timestamp: DateTime<Utc>,
    pub version: u32,
}

/// block 载荷:`crdt_update_b64` 是 base64(JSON(Vec<CellOp>)),
/// `operations` 是给审计日志看的人类可读摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockPayload {
    pub crdt_update_b64: String,
    pub operations: Vec<String>,
}

/// 区块链式审计单元:签名覆盖 header + payload + merkle_root。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub payload: BlockPayload,
    pub merkle_root: String,
    pub signature: String,
}

/// merkle_root = SHA256(canonical payload json)。
fn payload_merkle(payload: &BlockPayload) -> String {
    let payload_json = serde_json::to_string(payload).unwrap_or_default();
    format!("{:x}", Sha256::digest(payload_json.as_bytes()))
}

impl Block {
    /// 用一批 [`CellOp`] 构造并签名一个 block。
    ///
    /// `prev_hash` 是作者在该文档上的本地链头;不同作者的链构成 DAG,
    /// 合并语义由 [`CellOp`] 的 LWW 规则决定,与 block 顺序无关。
    pub fn from_ops(
        doc_id: &str,
        prev_hash: Option<String>,
        author: &str,
        signing_key: &SigningKey,
        ops: &[CellOp],
    ) -> Self {
        let ops_json = serde_json::to_vec(ops).unwrap_or_default();
        let payload = BlockPayload {
            crdt_update_b64: base64::engine::general_purpose::STANDARD.encode(&ops_json),
            operations: ops.iter().map(CellOp::describe).collect(),
        };
        let merkle_root = payload_merkle(&payload);
        let author_pubkey = format!(
            "ed25519:{}",
            hex::encode(signing_key.verifying_key().to_bytes())
        );
        let header = BlockHeader {
            doc_id: doc_id.to_string(),
            prev_hash,
            author: author.to_string(),
            author_pubkey,
            timestamp: Utc::now(),
            version: 1,
        };
        let serialized = serde_json::to_string(&(&header, &payload, &merkle_root)).unwrap();
        let signature = hex::encode(signing_key.sign(serialized.as_bytes()).to_bytes());
        Self {
            header,
            payload,
            merkle_root,
            signature,
        }
    }

    /// block 唯一键:SHA256(doc_id:timestamp:merkle_root:signature)。
    /// 签名唯一 ⇒ hash 唯一,重复收到同一 block 时天然去重。
    pub fn hash_key(&self) -> String {
        let hash_input = format!(
            "{}:{}:{}:{}",
            self.header.doc_id,
            self.header.timestamp.to_rfc3339(),
            self.merkle_root,
            self.signature
        );
        format!("{:x}", Sha256::digest(hash_input.as_bytes()))
    }

    /// merkle_root 是否与载荷一致(载荷被篡改会破坏该等式)。
    pub fn verify_merkle(&self) -> bool {
        self.merkle_root == payload_merkle(&self.payload)
    }

    /// 全量校验:merkle + 签名。
    pub fn verify(&self) -> bool {
        if !self.verify_merkle() {
            return false;
        }
        let Some(public_key) = self.header.author_pubkey.strip_prefix("ed25519:") else {
            return false;
        };
        let Ok(decoded) = hex::decode(public_key) else {
            return false;
        };
        let Ok(key_bytes) = <[u8; 32]>::try_from(decoded.as_slice()) else {
            return false;
        };
        let Ok(public_key) = VerifyingKey::from_bytes(&key_bytes) else {
            return false;
        };
        let Ok(sig_bytes) = hex::decode(&self.signature) else {
            return false;
        };
        let Ok(sig) = ed25519_dalek::Signature::from_slice(&sig_bytes) else {
            return false;
        };
        let serialized =
            match serde_json::to_string(&(&self.header, &self.payload, &self.merkle_root)) {
                Ok(s) => s,
                Err(_) => return false,
            };
        public_key.verify(serialized.as_bytes(), &sig).is_ok()
    }

    /// 解出载荷里的 LWW 操作。
    pub fn decoded_ops(&self) -> Result<Vec<CellOp>> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&self.payload.crdt_update_b64)
            .map_err(|e| Error::InvalidBlock(format!("bad base64 payload: {e}")))?;
        serde_json::from_slice(&raw).map_err(|e| Error::InvalidBlock(format!("bad ops json: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{OpKind, OpStamp};

    fn sample_ops(author: &str, ts: i64) -> Vec<CellOp> {
        vec![
            CellOp::new(
                ts,
                author,
                OpKind::SetCell {
                    sheet: "Sheet1".into(),
                    row: 1,
                    col: 1,
                    value: Some("hello".into()),
                    formula: None,
                    style: None,
                },
            ),
            CellOp::new(
                ts,
                author,
                OpKind::AddSheet {
                    sheet: "Sheet1".into(),
                    position: None,
                },
            ),
        ]
    }

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn sign_then_verify_ok() {
        let block = Block::from_ops("doc-1", None, "alice", &key(), &sample_ops("alice", 100));
        assert!(block.verify());
        assert!(block.verify_merkle());
    }

    #[test]
    fn tamper_payload_breaks_verification() {
        let mut block = Block::from_ops("doc-1", None, "alice", &key(), &sample_ops("alice", 100));
        block.payload.operations.push("injected".into());
        assert!(!block.verify(), "改载荷不重签必须被识破");
    }

    #[test]
    fn tamper_signature_breaks_verification() {
        let mut block = Block::from_ops("doc-1", None, "alice", &key(), &sample_ops("alice", 100));
        let flipped = if block.signature.starts_with('a') {
            format!("b{}", &block.signature[1..])
        } else {
            format!("a{}", &block.signature[1..])
        };
        block.signature = flipped;
        assert!(!block.verify());
    }

    #[test]
    fn hash_is_unique_and_stable() {
        let b1 = Block::from_ops("doc-1", None, "alice", &key(), &sample_ops("alice", 100));
        let b2: Block = serde_json::from_slice(&serde_json::to_vec(&b1).unwrap()).unwrap();
        assert_eq!(b1.hash_key(), b2.hash_key(), "同内容 hash 稳定(去重依赖它)");
        let b3 = Block::from_ops("doc-1", None, "alice", &key(), &sample_ops("alice", 101));
        assert_ne!(b1.hash_key(), b3.hash_key());
    }

    #[test]
    fn ops_roundtrip() {
        let ops = sample_ops("alice", 42);
        let block = Block::from_ops("doc-1", None, "alice", &key(), &ops);
        assert_eq!(block.decoded_ops().unwrap(), ops);
    }

    #[test]
    fn foreign_pubkey_fails() {
        let block = Block::from_ops("doc-1", None, "alice", &key(), &sample_ops("alice", 100));
        let mut forged = block.clone();
        forged.header.author = "mallory".into();
        assert!(!forged.verify(), "换 author 不重签必须失败");
    }

    #[test]
    fn stamp_in_ops_is_lww_comparable() {
        let ops = sample_ops("alice", 5);
        assert_eq!(ops[0].stamp, OpStamp::new(5, "alice"));
    }
}
