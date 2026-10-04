# MeshSExcel

MeshSExcel 是一个面向局域网内企业协作的在线办公表格设计与原型工程仓库，聚焦在：

- 局域网 P2P 同步
- 表格编辑与实时协作
- 区块链式审计链 / 不可篡改变更日志
- 高性能大表计算与汇总
- WPS / Excel / LibreOffice 兼容导出
- 桌面端与浏览器端 UI 设计
- 安全、权限、鲁棒性与可恢复性

本仓库既包含整体软件设计文档，也包含一个最小可运行的 PoC：PoC B — Rust 多节点局域网 gossip demo，演示 Block 生成、签名、校验与节点间同步。

## 目录

- `PROJECT_ARCHIVE_INDEX.md` — 主入口文档，汇总整个项目设计与实现说明
- `spec/` — API、协议、schema 与数据模型设计
- `poc-b-rust/` — Rust 多节点 demo（局域网 P2P + Block + RocksDB）
- `docs/` — 可用轮子、参考项目与开发建议
- `LICENSE` — 采用 artboard 的 ACL-1.0 自定义协议

## 主要特色

- 局域网内无需中心服务器即可相互发现并同步数据
- 每个节点保留自己的数据存档与审计链
- 支持轻量快照 + 增量 Block 恢复
- 建议使用 CRDT 进行实时协作，Block 用于不可篡改审计
- 面向大规模表格与复杂公式场景，支持增量重算与汇总缓存

## 适合的路线

- PoC 版：先验证 P2P + Block + 本地存档
- MVP 版：接入 Yjs / HyperFormula / SheetJS
- 企业版：加入权限、LDAP、分布式查询、监控与备份体系

## 下载/归档方式

本仓库会在 GitHub 上生成 ZIP 下载包：

- 进入仓库页面
- 点击 `Code` → `Download ZIP`

如果你希望进一步扩展成真正的产品原型，可以接着从 `PROJECT_ARCHIVE_INDEX.md` 进入详细设计与 PoC 实现。

## License

本仓库使用与 artboard 一致的自定义协议：ACL-1.0（Artboard Community License, Version 1.0）。

完整协议见 `LICENSE`。
