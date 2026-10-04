[package]
name = "meshsexcel-poc-b"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1.38", features = ["full"] }
futures = "0.3"
anyhow = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
chrono = { version = "0.4", features = ["serde"] }
sha2 = "0.10"
rand = "0.8"
hex = "0.4"
base64 = "0.21"
ed25519-dalek = { version = "2.1", features = ["rand_core"] }
rocksdb = "0.22"
libp2p = { version = "0.53.2", features = [
  "tcp",
  "noise",
  "yamux",
  "mdns",
  "gossipsub",
  "identify",
  "macros",
  "serde",
  "tokio"
] }
clap = { version = "4.5.4", features = ["derive"] }

[profile.release]
debug = false
lto = true
codegen-units = 1
