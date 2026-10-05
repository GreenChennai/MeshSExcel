# MeshSExcel 设计与协议说明

本目录收录了该项目的核心接口与数据模型设计。

- `openapi.yaml`:本地节点 REST API 草案(`meshsexcel-node` 已实现其全部端点)
- `block.proto`:节点间 P2P 与 block 传输协议(protobuf 描述)
- `sqlite_schema.sql`:轻量桌面数据库结构(`meshsexcel-core` 的存储层按此建表)

相关设计思想:

- Block 负责审计和恢复
- 单元格级 LWW(Last-Writer-Wins)寄存器负责协同合并
- SQLite 负责本地存档(遵循 `sqlite_schema.sql`;RocksDB 留待高性能版)
- libp2p / gossipsub 负责局域网广播
