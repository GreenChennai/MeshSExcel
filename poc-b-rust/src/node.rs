//! PoC B 节点生命周期:每 5 秒产出一个演示 block,签名后本地入库并广播;
//! 同时接收 gossip 到来的 block,验签后入库。

use anyhow::Result;
use chrono::Utc;
use ed25519_dalek::{SigningKey, VerifyingKey};
use meshsexcel_core::Block;
use std::path::Path;
use std::time::Duration;
use tokio::time;

use crate::db::BlockStore;
use crate::network::{NetEvent, NetworkConfig, NetworkNode};

pub struct LocalNode {
    pub node_id: String,
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
    pub store: BlockStore,
    pub network: NetworkNode,
    pub current_head: Option<String>,
}

impl LocalNode {
    pub async fn new(node_id: &str, db_dir: &str, port: u16) -> Result<Self> {
        let db_path = Path::new(db_dir);
        if !db_path.exists() {
            std::fs::create_dir_all(db_path)?;
        }

        let store = BlockStore::open(db_path)?;
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let verifying_key = signing_key.verifying_key();

        let network = NetworkNode::new(NetworkConfig {
            node_id: node_id.to_string(),
            listen_addr: format!("/ip4/0.0.0.0/tcp/{port}/").parse()?,
            topic: "lansheet-demo".to_string(),
            on_discovered: None,
        })?;

        let current_head = store.latest_block("sheet-demo")?.map(|b| b.hash_key());

        let node = Self {
            node_id: node_id.to_string(),
            signing_key,
            verifying_key,
            store,
            network,
            current_head,
        };
        println!(
            "[{}] identity ed25519:{}",
            node.node_id,
            hex::encode(node.verifying_key.to_bytes())
        );
        Ok(node)
    }

    pub async fn run(&mut self) -> Result<()> {
        let mut interval = time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let block = self.make_block();
                    self.handle_incoming_block(&block)?;
                    println!("[{}] generated block hash: {}", self.node_id, block.hash_key());
                    self.network.publish_block(&serde_json::to_vec(&block)?).await?;
                    self.current_head = Some(block.hash_key());
                }
                maybe_event = self.network.next_event() => {
                    match maybe_event {
                        Ok(NetEvent::Block(bytes)) => {
                            let parsed: Block = match serde_json::from_slice(&bytes) {
                                Ok(v) => v,
                                Err(err) => {
                                    eprintln!("[{}] invalid block json: {}", self.node_id, err);
                                    continue;
                                }
                            };

                            if !parsed.verify() {
                                eprintln!("[{}] invalid block signature", self.node_id);
                                continue;
                            }

                            let hash = parsed.hash_key();
                            if self.store.contains(&parsed.header.doc_id, &hash)? {
                                println!("[{}] duplicate block ignored: {}", self.node_id, hash);
                                continue;
                            }

                            println!(
                                "[{}] received valid block {} from {}",
                                self.node_id, hash, parsed.header.author
                            );
                            self.store.put_block(&parsed)?;
                        }
                        // PoC 每 5 秒产新块,无需 mesh 成形补发
                        Ok(NetEvent::MeshReady) => {}
                        Err(err) => eprintln!("[{}] network event error: {}", self.node_id, err),
                    }
                }
            }
        }
    }

    fn handle_incoming_block(&mut self, block: &Block) -> Result<()> {
        if !block.verify() {
            anyhow::bail!("block verify failed");
        }
        self.store.put_block(block)?;
        Ok(())
    }

    fn make_block(&mut self) -> Block {
        let doc_id = "sheet-demo";
        let prev_hash = self.current_head.clone();
        let ops = vec![meshsexcel_core::CellOp::new(
            Utc::now().timestamp_millis(),
            self.node_id.clone(),
            meshsexcel_core::OpKind::SetCell {
                sheet: "sheet-demo".into(),
                row: 1,
                col: 1,
                value: Some(format!("demo-update-{}", Utc::now().timestamp_millis())),
                formula: None,
                style: None,
            },
        )];

        Block::from_ops(doc_id, prev_hash, &self.node_id, &self.signing_key, &ops)
    }
}
