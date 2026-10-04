use anyhow::Result;
use libp2p::{
    gossipsub::{self, IdentTopic, MessageAuthenticity},
    mdns, noise, swarm::NetworkBehaviour, tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use std::time::Duration;

#[derive(NetworkBehaviour)]
pub struct AppBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
}

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub node_id: String,
    pub listen_addr: Multiaddr,
    pub topic: String,
}

pub struct NetworkNode {
    pub swarm: Swarm<AppBehaviour>,
    pub topic: IdentTopic,
    pub node_id: String,
}

impl NetworkNode {
    pub async fn new(cfg: NetworkConfig) -> Result<Self> {
        let id_keys = libp2p::identity::Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(id_keys.public());

        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .validation_mode(gossipsub::ValidationMode::Strict)
            .heartbeat_interval(Duration::from_secs(10))
            .build()
            .expect("valid config");

        let swarm = SwarmBuilder::with_async_std_executor(
            tcp::async_io::Transport::default()
                .upgrade(libp2p::core::upgrade::Version::V1Lazy)
                .authenticate(noise::Config::new(&id_keys)?)
                .multiplex(yamux::Config::default())
                .boxed(),
            AppBehaviour {
                gossipsub: gossipsub::Behaviour::new(
                    MessageAuthenticity::Signed(id_keys.clone()),
                    gossipsub_config,
                )?,
                mdns: mdns::tokio::Behaviour::new(mdns::Config::default(), local_peer_id)?,
            },
            local_peer_id,
        )
        .build();

        let topic = gossipsub::IdentTopic::new(cfg.topic.clone());
        let mut swarm = swarm;
        swarm.behaviour_mut().gossipsub.subscribe(&topic)?;
        swarm.listen_on(cfg.listen_addr.clone())?;

        Ok(Self {
            swarm,
            topic,
            node_id: cfg.node_id,
        })
    }

    pub async fn next_event(&mut self) -> Result<Vec<u8>> {
        loop {
            match self.swarm.select_next_some().await {
                libp2p::swarm::SwarmEvent::Behaviour(libp2p::swarm::dummy::BehaviourEvent::Gossipsub(
                    gossipsub::Event::Message { message, .. },
                )) => {
                    return Ok(message.data);
                }
                libp2p::swarm::SwarmEvent::Behaviour(libp2p::swarm::dummy::BehaviourEvent::Mdns(
                    mdns::Event::Discovered(list),
                )) => {
                    for (peer_id, multiaddr) in list {
                        self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                        self.swarm.connect_peer_id(&peer_id);
                        println!("[{}] discovered peer {} at {}", self.node_id, peer_id, multiaddr);
                    }
                }
                _ => {}
            }
        }
    }

    pub async fn publish_block(&mut self, block_json: &[u8]) -> Result<()> {
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), block_json.to_vec())?;
        Ok(())
    }
}
