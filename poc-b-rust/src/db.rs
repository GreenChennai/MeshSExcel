use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    pub doc_id: String,
    pub prev_hash: Option<String>,
    pub author: String,
    pub author_pubkey: String,
    pub timestamp: DateTime<Utc>,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockPayload {
    pub crdt_update_b64: String,
    pub operations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub payload: BlockPayload,
    pub merkle_root: String,
    pub signature: String,
}

impl Block {
    pub fn new(
        doc_id: &str,
        prev_hash: Option<String>,
        author: &str,
        author_pubkey: &str,
        crdt_update: &[u8],
        operations: Vec<String>,
        signing_key: &SigningKey,
    ) -> Self {
        let payload = BlockPayload {
            crdt_update_b64: base64::encode(crdt_update),
            operations,
        };

        let payload_json = serde_json::to_string(&payload).unwrap();
        let payload_hash = Sha256::digest(payload_json.as_bytes());
        let merkle_root = format!("{:x}", payload_hash);

        let header = BlockHeader {
            doc_id: doc_id.to_string(),
            prev_hash,
            author: author.to_string(),
            author_pubkey: author_pubkey.to_string(),
            timestamp: Utc::now(),
            version: 1,
        };

        let serialized = serde_json::to_string(&(&header, &payload, &merkle_root)).unwrap();
        let signature = signing_key.sign(serialized.as_bytes());
        let signature_hex = hex::encode(signature.to_bytes());

        Self {
            header,
            payload,
            merkle_root,
            signature: signature_hex,
        }
    }

    pub fn hash_key(&self) -> String {
        let hash_input = format!(
            "{}:{}:{}:{}",
            self.header.doc_id,
            self.header.timestamp.to_rfc3339(),
            self.merkle_root,
            self.signature
        );
        let digest = Sha256::digest(hash_input.as_bytes());
        format!("{:x}", digest)
    }

    pub fn verify(&self) -> bool {
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

        let serialized = serde_json::to_string(&(&self.header, &self.payload, &self.merkle_root)).unwrap();
        public_key.verify(serialized.as_bytes(), &sig).is_ok()
    }
}
