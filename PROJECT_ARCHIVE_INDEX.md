# MeshSExcel 项目制作合集

本仓库是关于 “局域网内在线办公表格编辑器” 的完整设计与实现归档，包含：

- 一个主入口设计文档：`PROJECT_ARCHIVE_INDEX.md`
- 技术细节说明：API、消息格式、数据库 schema、实时同步设计
- PoC B：Rust 多节点局域网 gossip demo
- 参考轮子与开源项目清单：`docs/REFERENCE_WHEELS.md`

## 目录说明

- `README.md`：仓库总览
- `PROJECT_ARCHIVE_INDEX.md`：主 Markdown 入口，概括项目创意、设计、架构、功能、技术栈
- `spec/`：API / Protobuf / SQL schema / 消息结构
- `poc-b-rust/`：可运行的 Rust demo
- `docs/`：参考项目与快速开发建议
- `LICENSE`：Artboard 社区开源协议（ACL-1.0）

## 设计目标

MeshSExcel 目标是构建一套适用于局域网办公场景的在线表格应用，满足以下要求：

- 局域网内多台机器直接协作，低延迟同步
- 每台机器保留一份数据存档，不因单点硬盘故障导致数据丢失
- 区块链式变更链记录历史变更与不可篡改审计
- 支持大规模数据、统计汇总、实时公式重算
- 兼容 Excel / WPS 的文件格式
- 具备良好的 UI、权限、安全与容错能力

## 代码与技术概述

### 1. 总体架构

推荐采用：

- 前端：React + TypeScript + Vite
- 表格/计算：HyperFormula + Handsontable / AG Grid
- 协同：Yjs / Automerge（CRDT）
- 本地节点：Rust / Go / Node.js
- 网络：libp2p + mdns + gossipsub
- 存储：RocksDB / SQLite
- 导入导出：SheetJS / Apache POI / OpenXML SDK

### 2. 核心能力

- 每次编辑上链为 block
- block 通过 P2P gossip 网络广播
- 节点校验签名、prev_hash 与 merkle_root
- 数据保存在本地 RocksDB/SQLite 中
- 计算层支持增量公式重算
- UI 侧使用虚拟化大表渲染

### 3. 为了快速迭代选择的工程组合

推荐 PoC / MVP 的工程组合：

- Rust 节点（P2P + block + storage）
- React 前端（可视化与表格 UI）
- Yjs + HyperFormula
- SheetJS 导入导出
- libp2p / mDNS / Gossipsub

## 参考开源项目

详细见：`docs/REFERENCE_WHEELS.md`

常见轮子：

- libp2p
- Yjs
- HyperFormula
- RocksDB
- SheetJS
- Handsontable / AG Grid
- Wasmtime / Wasmer
- Prometheus + Grafana
- Tantivy / Bleve / Faiss

## 该仓库的产出目标

本仓库可以作为：

- 软件方案说明书
- 产品 demo 原型服务
- 研发任务分解文档
- 初期工程架构设计归档
- 先行 PoC 执行方案

## 下一步建议

建议按顺序推进：

1. PoC B 完整运行与验证
2. 设计表格 CRDT + 公式计算试验
3. 设计 UI 与 XLSX 导入导出接口
4. 启动多节点网络实验
5. 打通安全认证、权限管理与快照恢复

---

如果你需要下一步，我可以继续为你生成：

- 完整的前端项目骨架（React + TypeScript + Yjs + HyperFormula）
- 后端表格节点代码（Rust/Go）
- 容器化部署方案（Docker / docker-compose）
- Excel/WPS 导入导出兼容方案
- 插件系统原型设计
