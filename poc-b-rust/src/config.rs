# PoC B: Rust 多节点局域网 gossip / block demo

这个 PoC 展示：

- 多节点使用 libp2p 自动发现
- 节点之间利用 gossipsub 传播 block
- 每个 block 均包含签名与校验
- 生成后写入 RocksDB
- 节点离线后重新启动，即可从本地存档恢复状态

## 运行

```bash
cargo run -- --node-id alice --db-dir ./tmp/alice --port 20001
cargo run -- --node-id bob --db-dir ./tmp/bob --port 20002
cargo run -- --node-id charlie --db-dir ./tmp/charlie --port 20003
```

你可以在同一个局域网上运行多个实例。

## 结构

- `src/config.rs`：命令行参数
- `src/block.rs`：block 结构与验证逻辑
- `src/db.rs`：RocksDB 持久化层
- `src/network.rs`：libp2p + mDNS + gossipsub
- `src/node.rs`：节点生命周期和 block 生成
- `src/main.rs`：入口

