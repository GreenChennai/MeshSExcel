//! gossip 网络任务:mDNS 发现 → 登记 peers → dial;gossipsub 收 block;
//! 发布命令来自 `AppState.publish_tx`。

use crate::state::{AcceptOutcome, AppState};
use anyhow::Result;
use libp2p::Multiaddr;
use meshsexcel_core::Block;
use meshsexcel_net::{NetEvent, NetworkConfig, NetworkNode};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

/// 启动网络任务(持有发布通道的接收端)。
pub fn spawn(
    node_id: &str,
    p2p_port: u16,
    topic: &str,
    app: Arc<AppState>,
    mut publish_rx: UnboundedReceiver<Vec<u8>>,
) -> Result<JoinHandle<()>> {
    let peer_log = Arc::clone(&app.store);
    let cfg = NetworkConfig {
        node_id: node_id.to_string(),
        listen_addr: format!("/ip4/0.0.0.0/tcp/{p2p_port}")
            .parse::<Multiaddr>()
            .map_err(|e| anyhow::anyhow!("bad listen addr: {e}"))?,
        topic: topic.to_string(),
        on_discovered: Some(Arc::new(
            move |peer_id: libp2p::PeerId, addr: &Multiaddr| {
                if let Err(e) =
                    peer_log.upsert_peer(&peer_id.to_string(), Some(&addr.to_string()), None)
                {
                    eprintln!("peer log failed: {e}");
                }
            },
        )),
    };
    let mut net = NetworkNode::new(cfg)?;

    let node_id = node_id.to_string();
    let handle = tokio::spawn(async move {
        println!("[{node_id}] p2p task started (mdns+gossipsub)");
        loop {
            tokio::select! {
                maybe = net.next_event() => match maybe {
                    Ok(NetEvent::Block(data)) => match serde_json::from_slice::<Block>(&data) {
                        Ok(block) => match app.ingest(block, false) {
                            Ok(AcceptOutcome::Applied) => {
                                println!("[{node_id}] block applied via gossip");
                            }
                            Ok(AcceptOutcome::Duplicate) => {}
                            Ok(AcceptOutcome::Rejected(reason)) => {
                                println!("[{node_id}] block rejected: {reason}");
                            }
                            Err(e) => println!("[{node_id}] ingest error: {e}"),
                        },
                        Err(e) => println!("[{node_id}] bad gossip payload: {e}"),
                    },
                    Ok(NetEvent::MeshReady) => {
                        // mesh 成形(或重发窗口到):把本地全部 block 重广播,
                        // 后加入的节点借此补齐;对端按消息 id 去重,无放大
                        match app.all_blocks() {
                            Ok(blocks) => {
                                println!(
                                    "[{node_id}] mesh ready; announcing {} blocks",
                                    blocks.len()
                                );
                                for block in blocks {
                                    if let Ok(bytes) = serde_json::to_vec(&block) {
                                        if let Err(e) = net.publish_block(&bytes).await {
                                            println!("[{node_id}] announce failed: {e}");
                                        }
                                    }
                                }
                            }
                            Err(e) => println!("[{node_id}] collect blocks failed: {e}"),
                        }
                    }
                    Err(e) => println!("[{node_id}] network error: {e}"),
                },
                Some(bytes) = publish_rx.recv() => {
                    if let Err(e) = net.publish_block(&bytes).await {
                        println!("[{node_id}] publish failed: {e}");
                    }
                }
            }
        }
    });
    Ok(handle)
}
