# MeshSExcel

MeshSExcel 是一个面向局域网内企业协作的在线办公表格软件,聚焦在:

- 局域网 P2P 同步(mDNS 自动发现 + gossipsub 广播)
- 表格编辑与实时协作(单元格级 LWW 合并,乱序到达也收敛)
- 区块链式审计链 / 不可篡改变更日志(ed25519 签名,逐块校验)
- 公式计算(SUM/AVERAGE/IF 等,依赖图增量重算)
- XLSX / CSV 导入导出(公式写成真公式,WPS / Excel 可直接重算)
- 单二进制交付:REST API + 内嵌 Web UI + SQLite 存档

## 当前实现状态(2026-10)

按 `PROJECT_ARCHIVE_INDEX.md` 的推进顺序:

| 阶段 | 状态 | 说明 |
|------|------|------|
| 第一阶段:PoC(P2P + Block + 存档) | ✅ 完成 | `poc-b-rust`,多节点 mDNS/gossipsub 演示 |
| 第二阶段:MVP(表格 + 公式 + UI + XLSX) | ✅ 完成 | `crates/meshsexcel-*`,见下方"运行" |
| 第三阶段:企业版(权限 / LDAP / 监控) | 规划中 | 见 PROJECT_ARCHIVE_INDEX 第十一节 |

工程说明:

- `meshsexcel-core`:block 审计链、LWW 操作合并、公式引擎、SQLite 存储层(按 `spec/sqlite_schema.sql` 建表)、XLSX/CSV 导入导出
- `meshsexcel-net`:libp2p + mDNS + gossipsub 网络栈(PoC 与正式节点共用)
- `meshsexcel-node`:局域网表格节点(axum REST API + 内嵌 Web UI)
- `poc-b-rust`:最小多节点演示(与正式节点同一套 block/网络语义)
- `spec/`:REST API(openapi.yaml)、P2P 协议(block.proto)、存储 schema(sqlite_schema.sql)

> 原脚手架的 RocksDB 属"高性能核心"规划;MVP 按规范里的轻量桌面路线用 SQLite 落地。
> 协同层按文档建议采用 CRDT(单元格级 LWW 寄存器);HyperFormula/Yjs(前端 JS 生态)
> 替换为 Rust 内置公式引擎 + 内嵌 UI,换来零构建链、单二进制局域网部署。

## 运行

```bash
# 节点 A(本机)
cargo run -p meshsexcel-node -- --node-id alice --db-dir ./data/alice --http-port 8443 --p2p-port 20001

# 节点 B(局域网另一台机器,或本机另开端口)
cargo run -p meshsexcel-node -- --node-id bob --db-dir ./data/bob --http-port 8444 --p2p-port 20002
```

浏览器打开 `http://localhost:8443`:

1. 「新建文档」建一张表;编辑单元格、输入 `=SUM(A1:A9)` 等公式,自动保存并上链
2. 第二个节点启动后自动互发现(右上角「节点」面板可见),文档与变更自动同步
3. 「审计历史」查看 block 链(作者 / 时间 / 操作摘要 / hash)
4. 「快照」可备份整簿并随时恢复;「导入/导出」支持 XLSX 与 CSV

PoC B(纯网络演示,无 UI):

```bash
cargo run -p meshsexcel-poc-b -- --node-id alice --db-dir ./tmp/alice --port 20001
```

## REST API

实现 `spec/openapi.yaml` 全部端点,另有供 Web UI 使用的扩展端点(cells 网格、
ops 批量提交、快照、导入),详见 `spec/README.md`。

## 开发

```bash
cargo test --workspace      # 单元 + API 集成测试
cargo clippy --all-targets -- -D warnings
cargo build --release       # 产物:target/release/meshsexcel-node.exe
```

## 目录

- `PROJECT_ARCHIVE_INDEX.md` — 主入口文档,整体设计、架构与推进路线
- `spec/` — API、协议、schema 与数据模型设计
- `crates/` — 正式实现(core / net / node)
- `poc-b-rust/` — 多节点网络 PoC
- `docs/` — 可用轮子、参考项目与开发建议
- `LICENSE` — artboard 的 ACL-1.0 自定义协议

## License

本仓库使用与 artboard 一致的自定义协议:ACL-1.0(Artboard Community License, Version 1.0)。

完整协议见 `LICENSE`。
