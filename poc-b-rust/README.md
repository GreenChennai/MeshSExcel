# PoC B:Rust 多节点局域网 gossip / block demo

这个 PoC 展示:

- 多节点使用 libp2p 自动发现(mDNS)
- 节点之间利用 gossipsub 传播 block
- 每个 block 均包含签名与校验(与正式节点共用 `meshsexcel-core` / `meshsexcel-net`)
- 生成后写入 SQLite(遵循 `spec/sqlite_schema.sql` 的 blocks 表)
- 节点离线后重新启动,即可从本地存档恢复状态(链头续接)

> 原始脚手架里写的 RocksDB 属于"高性能核心"规划;MVP 阶段按
> `spec/sqlite_schema.sql` 用 SQLite 落地,接口保持一致,后续可平替。

## 运行

```bash
cargo run -p meshsexcel-poc-b -- --node-id alice --db-dir ./tmp/alice --port 20001
cargo run -p meshsexcel-poc-b -- --node-id bob --db-dir ./tmp/bob --port 20002
cargo run -p meshsexcel-poc-b -- --node-id charlie --db-dir ./tmp/charlie --port 20003
```

你可以在同一个局域网上运行多个实例;同一台机器上跑多个实例也可以
(mDNS 会互相发现,gossipsub 会组成 mesh,各节点每 5 秒产出一个演示 block)。

## 结构

- `src/config.rs`:命令行参数
- `src/block.rs`:block 结构与验证逻辑(实现在 `meshsexcel-core`,此处薄再导出)
- `src/db.rs`:SQLite 持久化层
- `src/network.rs`:libp2p + mDNS + gossipsub(实现在 `meshsexcel-net`,此处薄再导出)
- `src/node.rs`:节点生命周期和 block 生成
- `src/main.rs`:入口
