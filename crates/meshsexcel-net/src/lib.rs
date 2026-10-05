//! MeshSExcel 网络栈:libp2p + mDNS + gossipsub。
//!
//! PoC B 与正式节点共用这一份实现,保证网络语义一致:
//! - mDNS 自动发现 → dial + gossipsub 显式 peer
//! - gossipsub(Strict 签名模式)广播 block JSON
//! - mesh 未成形时发布静默跳过(本地已入库)
//! - mesh 成形(以及之后每 30s 的重发窗口)由 [`NetEvent::MeshReady`]
//!   通知上层,上层把本地全部 block 重广播一遍,后加入的节点借此补齐;
//!   gossipsub 按消息 id 去重,对已有这些 block 的节点没有放大开销

use anyhow::Result;
use futures::StreamExt;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{
    gossipsub::{self, IdentTopic, MessageAuthenticity, TopicHash},
    mdns, noise, tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// gossip 消息体上限(2MB:足够容纳整块 XLSX 导入的 ops)。
const MAX_TRANSMIT_SIZE: usize = 2 * 1024 * 1024;

/// MeshReady 重广播窗口:同一窗口内不重复触发。
const MESH_READY_WINDOW: Duration = Duration::from_secs(30);

/// mDNS 发现新 peer 时的回调(节点用它登记 peers 表)。
pub type DiscoveredCallback = Arc<dyn Fn(PeerId, &Multiaddr) + Send + Sync>;

#[derive(Clone)]
pub struct NetworkConfig {
    pub node_id: String,
    pub listen_addr: Multiaddr,
    pub topic: String,
    /// 可为 None。
    pub on_discovered: Option<DiscoveredCallback>,
}

/// 网络事件:上层在事件循环里处理。
#[derive(Debug)]
pub enum NetEvent {
    /// 收到一个 gossip block(JSON 字节)。
    Block(Vec<u8>),
    /// gossip mesh 已成形(首个 peer 订阅;之后每 MESH_READY_WINDOW 重复一次),
    /// 上层应把本地全部 block 重广播一遍以补齐后加入的节点。
    MeshReady,
}

#[derive(NetworkBehaviour)]
pub struct MeshBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
}

pub struct NetworkNode {
    swarm: Swarm<MeshBehaviour>,
    topic: IdentTopic,
    topic_hash: TopicHash,
    node_id: String,
    on_discovered: Option<DiscoveredCallback>,
    last_mesh_ready: Option<Instant>,
}

impl NetworkNode {
    pub fn new(cfg: NetworkConfig) -> Result<Self> {
        let mut swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_behaviour(|key| {
                let gossipsub_config = gossipsub::ConfigBuilder::default()
                    .validation_mode(gossipsub::ValidationMode::Strict)
                    .heartbeat_interval(Duration::from_secs(1))
                    .max_transmit_size(MAX_TRANSMIT_SIZE)
                    .build()
                    .map_err(std::io::Error::other)?;
                let gossipsub = gossipsub::Behaviour::new(
                    MessageAuthenticity::Signed(key.clone()),
                    gossipsub_config,
                )?;
                let mdns = mdns::tokio::Behaviour::new(
                    mdns::Config::default(),
                    key.public().to_peer_id(),
                )?;
                Ok(MeshBehaviour { gossipsub, mdns })
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(300)))
            .build();

        let topic = IdentTopic::new(cfg.topic.clone());
        let topic_hash = topic.hash();
        swarm.behaviour_mut().gossipsub.subscribe(&topic)?;
        swarm.listen_on(cfg.listen_addr.clone())?;

        Ok(Self {
            swarm,
            topic,
            topic_hash,
            node_id: cfg.node_id,
            on_discovered: cfg.on_discovered,
            last_mesh_ready: None,
        })
    }

    /// 阻塞取下一条网络事件;mDNS 发现 / 监听事件在内部就地处理。
    pub async fn next_event(&mut self) -> Result<NetEvent> {
        loop {
            match self.swarm.select_next_some().await {
                SwarmEvent::Behaviour(MeshBehaviourEvent::Gossipsub(
                    gossipsub::Event::Message { message, .. },
                )) => return Ok(NetEvent::Block(message.data)),
                SwarmEvent::Behaviour(MeshBehaviourEvent::Gossipsub(
                    gossipsub::Event::Subscribed { topic, .. },
                )) => {
                    if topic == self.topic_hash && self.mesh_ready_due() {
                        return Ok(NetEvent::MeshReady);
                    }
                }
                SwarmEvent::Behaviour(MeshBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                    for (peer_id, multiaddr) in list {
                        self.swarm
                            .behaviour_mut()
                            .gossipsub
                            .add_explicit_peer(&peer_id);
                        let _ = self.swarm.dial(multiaddr.clone());
                        if let Some(cb) = &self.on_discovered {
                            cb(peer_id, &multiaddr);
                        }
                        println!(
                            "[{}] discovered peer {} at {}",
                            self.node_id, peer_id, multiaddr
                        );
                    }
                }
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("[{}] p2p listening on {}", self.node_id, address);
                }
                _ => {}
            }
        }
    }

    fn mesh_ready_due(&mut self) -> bool {
        let due = match self.last_mesh_ready {
            None => true,
            Some(t) => t.elapsed() >= MESH_READY_WINDOW,
        };
        if due {
            self.last_mesh_ready = Some(Instant::now());
        }
        due
    }

    /// 广播 block;mesh 未成形(无已连接订阅者)时静默跳过
    /// (MeshReady 事件会让上层补发)。
    pub async fn publish_block(&mut self, block_json: &[u8]) -> Result<()> {
        match self
            .swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), block_json.to_vec())
        {
            Ok(_) => {}
            Err(gossipsub::PublishError::InsufficientPeers) => {
                println!(
                    "[{}] no gossip peers yet; block kept locally only",
                    self.node_id
                );
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
}
