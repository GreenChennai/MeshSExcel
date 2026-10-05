# MeshSExcel — 局域网在线办公表格软件设计与 PoC 制作合集

版本:1.0(2026-10-04)/ 实现状态更新:2026-10-05
作者:GreenChennai / Copilot

## 一、项目概述

MeshSExcel 是一个面向局域网环境的在线办公表格产品方案,目标是在局域网内运行丰富的表格编辑与统计分析功能,同时保留"区块链式"审计与不可篡改特性,允许每台安装了本软件的节点在本地保留一份数据存档。

它不是简单的在线网页表格,而是一个"本地节点 + 局域网 P2P + 安全审计 + 大表计算 + 协作图形界面"的统一架构。

### 核心诉求

- 高性能:大量数据与非常多的汇总统计公式实时计算
- 安全:每次修改进行签名与审计,节点可验证完整性
- 鲁棒:单机故障不导致数据丢失
- 协作:局域网内多台机器同时编辑并保持一致性
- 兼容:可导入导出 Excel / WPS 兼容格式
- 体验:界面可美观、符合 Office / WPS 习惯

## 二、软件名称与定位

软件名称:MeshSExcel

含义:

- Mesh:网状 / 局域网 P2P 连接
- Excel:表格 / 电子表格
- 用于让人第一眼理解这是一个"局域网内协同型电子表格系统"

适用场景:

- 企业内部小型办公网表格
- 车间 / 工厂 / 研究所 / 团队协作
- 多台电脑共享账本、统计、调度、报表

## 三、设计目标

### 1) 数据可靠性

- 每个节点保留一份数据存档
- 每次变更生成 Block 并广播到其他节点
- 任意节点硬盘损坏后,使用其他节点副本恢复

### 2) 实时协作

- 多人同时编辑同一张表
- 行为落到 CRDT / 变更日志层
- 不需要集中式数据库来保证最终一致

### 3) 高性能

- 虚拟化渲染表格
- 依赖图增量计算公式
- 聚合/汇总缓存
- 后台异步计算大规模任务

### 4) 兼容性

- 支持 XLSX / CSV / ODS 等通用格式
- 尽可能兼容 MS Office / WPS 的公式和风格
- 导出结果可直接交付

### 5) 安全

- 节点双向认证
- 数据签名与区块校验
- 权限分层:owner/editor/readonly
- 可审计历史记录

## 四、技术栈建议

### 前端

- React + TypeScript
- Vite
- TailwindCSS / CSS modules
- Handsontable / AG Grid / 自研虚拟表格
- Yjs(协同状态)
- HyperFormula(公式计算)

### 后端 / 节点

- Rust(推荐生产级核心)
- Go(可替代)
- Node.js(可用于 PoC / 快速验证)

### 网络

- libp2p
- mdns discovery
- gossipsub
- 可选 relay / TURN(跨网段场景)

### 存储

- RocksDB(高性能核心)
- SQLite(轻量桌面模式)

### 文件兼容

- SheetJS
- Apache POI
- OpenXML SDK

### 安全 / 密钥

- ed25519-dalek
- ring / sodiumoxide
- OS Keychain / Vault / HSM

## 五、核心架构说明

### 1. 表格编辑层

- 用户在客户端编辑单元格、公式、样式、行列操作
- 变更变成增量操作
- 操作落到 CRDT 实例中
- 对外生成 block 或广播变更

### 2. 区块链式层

- 每次提交打包成 block
- block 包含:prev_hash、author、public_key、timestamp、payload、merkle_root、signature
- block 通过 P2P 广播给其他节点
- 其他节点校验 block:签名、链关系、payload 完整性

### 3. 存储层

- Local store keeps block history
- snapshot for full recovery
- index for aggregation and querying

### 4. 公式计算层

- 公式变更不需要全表重算
- 使用依赖图只重算受影响单元格
- 汇总操作在后台异步重算

## 六、关键功能清单

- 新建/打开/保存工作簿
- 表页管理(sheet)
- 单元格编辑 / 行列插入 / 删除
- 公式输入与自动计算
- 样式:字体、颜色、边框、对齐
- 条件格式
- 数据验证与保护
- 统计汇总与透视表
- 协作者状态与光标同步
- 文档版本历史
- 导入 XLSX/CSV
- 导出 XLSX/CSV/PDF(可选)
- 数据审计日志
- 权限管理
- 备份与恢复

## 七、接口与协议

本仓库中提供了以下设计:

- `spec/openapi.yaml`:REST API 接口定义(已实现)
- `spec/block.proto`:P2P / gRPC 相关的 block 协议
- `spec/sqlite_schema.sql`:SQLite 简化存储 schema(已实现,另补充 sheets 表)

## 八、PoC B:Rust 多节点 demo

仓库中包含 `poc-b-rust/`,它演示了:

- 局域网自动发现
- 多节点 gossip 广播
- Block 生成和签名
- Block 验证和持久化
- SQLite 存储

这可以作为后续表格系统的底座和演示基础。

## 九、参考轮子与工程选型

更多内容见 `docs/REFERENCE_WHEELS.md`。

### 关键轮子

- libp2p:网络与发现(已用)
- LWW CRDT:实时协同(已用单元格级实现)
- SQLite:轻量桌面存储(已用)
- calamine / rust_xlsxwriter:XLSX 导入导出(已用)
- Prometheus / Grafana:监控指标(未用)
- Wasmtime / Wasmer:插件与 Wasm(未用)

## 十、扩展能力

该项目可进一步扩展:

- 企业 LDAP / AD 集成
- 中继节点 / NAT 穿透方案
- 分布式查询与聚合分析
- 自定义公式函数插件
- 语义搜索与文档摘要
- 版本回溯与快照恢复

## 十一、推荐推进顺序

### 第一阶段:PoC

- 完成局域网 P2P 自动发现
- 实现 Block 生成与广播
- 验证签名与恢复

### 第二阶段:MVP

- 集成协同合并
- 接入公式计算
- 完成表格基础 UI
- 对 XLSX 导入导出

### 第三阶段:企业版

- 权限 / 登录 / 审计 / 备份
- 高性能表格渲染与索引
- 监控仪表盘与部署方案

## 十二、结论

MeshSExcel 并不是单纯做"一个表格应用",而是围绕"局域网内高可靠协同表格"构建出的一个结构化系统设计。它兼顾了:

- 数据安全
- 实时协同
- 高可用性
- 局域网部署
- Excel 兼容
- 大规模统计性能
- 可审计与不可篡改

这使其非常适合做企业内部办公场景的原型、demo 或后续正式产品的基座。

## 十三、实现状态(2026-10-05 更新)

按第十一节的推进顺序,当前进展:

- **第一阶段 PoC:已完成**。`poc-b-rust` 可多节点运行:mDNS 自动发现、
  gossipsub 广播、block 签名与校验、SQLite 持久化、重启后链头续接。
  网络栈抽为 `crates/meshsexcel-net`,PoC 与正式节点共用同一实现。
- **第二阶段 MVP:已完成(Rust 单二进制路线)**。`crates/meshsexcel-node` 提供:
  REST API(实现 `spec/openapi.yaml` 全部端点,另有 cells/ops/快照/导入 扩展端点)
  + 内嵌 Web 表格 UI(自研网格)+ 单元格级 LWW 协同合并 + 公式引擎
  (依赖图增量重算,SUM/AVERAGE/MIN/MAX/COUNT/IF/ROUND/CONCAT 等)+
  XLSX/CSV 导入导出(导出公式为真公式)+ block 审计链 + 快照备份恢复。
  技术选型说明:
  - 协同:文档建议的 CRDT 落地为单元格级 LWW(Last-Writer-Wins)寄存器,
    以 `(时间戳, 作者)` 全序定胜负,多节点乱序合并收敛有测试保证;
  - 表格/计算:文档建议的 HyperFormula/Yjs(前端 JS 生态)替换为 Rust 内置
    公式引擎与内嵌自研网格,换来零前端构建链、单二进制局域网交付;
  - 存储:按 `spec/sqlite_schema.sql` 用 SQLite(轻量桌面路线),
    RocksDB 留待高性能版;另补充 `sheets` 表管理表序。
- **第三阶段企业版:未开始**(权限 / LDAP / 监控 / 中继)。

## 附件

- `spec/openapi.yaml`
- `spec/block.proto`
- `spec/sqlite_schema.sql`
- `poc-b-rust/README.md`
- `docs/REFERENCE_WHEELS.md`
