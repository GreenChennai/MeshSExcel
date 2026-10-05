# MeshSExcel 桌面端:LibreOffice Calc 桥

用**真正的 LibreOffice Calc** 作为 MeshSExcel 的 Windows 本地编辑界面,
MeshSExcel 节点作为协同 / 审计链 / 存储底座:

```
┌─────────────────────┐  UNO API   ┌──────────────────┐  REST/ops  ┌───────────────┐
│ LibreOffice Calc     │ ◄────────► │ office-bridge     │ ◄────────► │ meshsexcel-node│
│ (完整办公体验,        │            │ (本目录,随 LO 自带 │            │ block链+gossip │
│  公式引擎原生重算)     │            │  的 Python 运行)   │            │ +SQLite 存档   │
└─────────────────────┘            └──────────────────┘            └───────┬───────┘
                                                                            │ gossipsub
                                                                    局域网其他 MeshSExcel 节点
```

工作方式:

- 桥以 LibreOffice 自带的 Python(`program\python.exe`,自带 UNO 模块)运行;
- 自动启动带 UNO 监听的 soffice 并新建文档窗口;
- **本地→远端**:轮询读取 Calc 已用区域(值 / 公式 / 加粗 / 字体色 / 填充色 /
  对齐),diff 出变更转成 CellOp 上链(节点签名 + gossip 广播);
- **远端→本地**:轮询 `/cells`,把其他节点的变更写回 Calc,公式由 Calc
  原生引擎重算;
- 工作表的新增 / 删除 / 重命名同样会同步(重命名按 删旧表+建新表+补格 处理)。

## 运行

前置:安装 [LibreOffice](https://www.libreoffice.org/)(任意 7.x+ 版本),并已有一个
meshsexcel-node 在运行(默认 `http://localhost:8443`)。

```cmd
:: 双击或命令行运行(自动定位 LibreOffice 的 python):
office-bridge\start-bridge.cmd

:: 指定节点与文档:
office-bridge\start-bridge.cmd --node http://192.168.1.10:8443 --doc doc-xxxx
```

启动后 Windows 桌面会弹出 LibreOffice Calc 窗口,像平常一样编辑即可——
每次变更自动生成签名 block 上链;局域网内其他节点(包括 Web 端)的修改
也会实时回显到这个窗口。

## 参数

| 参数 | 默认 | 说明 |
|------|------|------|
| `--node` | `http://localhost:8443` | 节点 REST 地址 |
| `--doc` | 最新文档(无则新建) | 要打开的文档 id |
| `--soffice-path` | 自动探测(注册表/常见路径) | soffice.exe 位置 |
| `--port` | 2002 | UNO 监听端口(多实例改端口) |
| `--interval` | 0.8 | 同步轮询间隔(秒) |

## 已知边界(MVP)

- 若 soffice 已在运行且未带 UNO 监听参数,桥无法连接——请先关闭所有
  LibreOffice 窗口再启动桥;
- 冲突语义为 LWW(后写者胜,以 block 时间戳定序);
- 单元格级样式同步 加粗 / 字体色 / 填充色 / 水平对齐;其余格式(字体、
  行高、条件格式等)暂不进链;
- Calc 里的公式由 Calc 原生重算;节点端有独立公式引擎用于 Web 端与导出,
  两侧对 SUM/AVERAGE/IF 等常用函数结果一致。
