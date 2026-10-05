#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""MeshSExcel 桌面桥:LibreOffice Calc(UNO)↔ MeshSExcel 节点(REST)。

以 LibreOffice 自带的 Python 运行(自带 uno 模块):
    "C:\\Program Files\\LibreOffice\\program\\python.exe" mesh_bridge.py --doc <doc_id>

职责:
  1. 启动(或复用)带 UNO 监听的 soffice,新建/打开文档窗口;
  2. 本地→远端:轮询读取 Calc 已用区域(值/公式/样式),diff 出变更
     转成 CellOp POST /ops,由节点签名上链并 gossip;
  3. 远端→本地:轮询 /cells,发现远端变更时把差异写回 Calc
     (setFormula / setString / 样式属性),公式由 Calc 引擎原生重算。

回声抑制:全部走"轮询 + 基线对账",写回远端数据后立即把基线更新为
远端状态,因此桥自己的写不会被判成用户编辑。
"""

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.request

# ---------------------------------------------------------------- 基础工具

# 代理免疫:本机常驻 Clash/TUN 类工具会劫持回环 HTTP,桥必须直连
_opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
urllib.request.install_opener(_opener)


def http_json(url, data=None, method=None, retries=3):
    last = None
    for attempt in range(retries):
        try:
            req = urllib.request.Request(url, data=data, method=method)
            if data:
                req.add_header("Content-Type", "application/json")
            with urllib.request.urlopen(req, timeout=10) as resp:
                body = resp.read()
                return json.loads(body) if body else None
        except Exception as exc:
            last = exc
            time.sleep(0.5 * (attempt + 1))
    raise last


def find_soffice(explicit):
    if explicit:
        return explicit
    try:
        import winreg
        key = winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\LibreOffice\UNO\InstallPath")
        path = winreg.QueryValueEx(key, "")[0]
        exe = os.path.join(path, "soffice.exe")
        if os.path.exists(exe):
            return exe
    except OSError:
        pass
    for candidate in (
        r"C:\Program Files\LibreOffice\program\soffice.exe",
        r"C:\Program Files (x86)\LibreOffice\program\soffice.exe",
        r"D:\LibreOffice\program\soffice.exe",
        r"D:\tools\LibreOffice\program\soffice.exe",
    ):
        if os.path.exists(candidate):
            return candidate
    raise SystemExit("找不到 soffice.exe,请用 --soffice-path 指定")


# ---------------------------------------------------------------- 规范形转换

def style_of(cell):
    """读取 UNO 单元格样式 → 我们的风格字典"""
    style = {}
    try:
        if cell.CharWeight and cell.CharWeight >= 150.0:
            style["bold"] = True
    except Exception:
        pass
    try:
        if cell.CharColor not in (None, -1):
            style["color"] = "#%06X" % cell.CharColor
    except Exception:
        pass
    try:
        if cell.CellBackColor not in (None, -1):
            style["bg"] = "#%06X" % cell.CellBackColor
    except Exception:
        pass
    try:
        h = cell.HoriJustify.value if hasattr(cell.HoriJustify, "value") else str(cell.HoriJustify)
        if h == "CENTER":
            style["align"] = "center"
        elif h == "RIGHT":
            style["align"] = "right"
    except Exception:
        pass
    return style or None


def cell_to_ours(cell):
    ours = {}
    formula = cell.getFormula()
    if formula:
        ours["formula"] = formula if formula.startswith("=") else "=" + formula
    text = cell.getString()
    if not formula and text not in (None, ""):
        ours["value"] = text
    style = style_of(cell)
    if style:
        ours["style"] = style
    return ours or None


def server_to_ours(c):
    ours = {}
    if c.get("formula"):
        # 公式格只保留公式;计算值由 Calc 原生重算,不参与比较(否则回声环)
        ours["formula"] = c["formula"]
    elif c.get("value") not in (None, ""):
        ours["value"] = str(c["value"])
    if c.get("style"):
        ours["style"] = c["style"]
    return ours or None


def canon(obj):
    return json.dumps(obj, sort_keys=True, ensure_ascii=False) if obj else ""


# ---------------------------------------------------------------- UNO 连接

def connect_uno(soffice, port):
    import uno

    def resolve():
        local = uno.getComponentContext()
        resolver = local.ServiceManager.createInstanceWithContext(
            "com.sun.star.bridge.UnoUrlResolver", local)
        return resolver.resolve(
            "uno:socket,host=127.0.0.1,port=%d;urp;StarOffice.ComponentContext" % port)

    try:
        ctx = resolve()
    except Exception:
        print("[bridge] 启动 LibreOffice(UNO 监听 :%d)…" % port)
        subprocess.Popen([
            soffice,
            "--accept=socket,host=127.0.0.1,port=%d;urp;StarOffice.ServiceManager" % port,
            "--norestore",
        ])
        ctx = None
        for _ in range(60):
            time.sleep(1)
            try:
                ctx = resolve()
                break
            except Exception:
                continue
        if ctx is None:
            raise SystemExit("无法连接 LibreOffice UNO(60s 超时)")
    smgr = ctx.ServiceManager
    desktop = smgr.createInstanceWithContext("com.sun.star.frame.Desktop", ctx)
    return ctx, desktop


def new_calc_doc(desktop):
    doc = desktop.loadComponentFromURL("private:factory/scalc", "_blank", 0, ())
    return doc


# ---------------------------------------------------------------- 样式写回

def apply_style(cell, style):
    from com.sun.star.table.CellHoriJustify import CENTER, RIGHT, LEFT  # noqa
    weight = cell.getPropertySetInfo().hasPropertyByName("CharWeight")
    if not weight:
        return
    cell.CharWeight = 150.0 if style.get("bold") else 100.0
    color = style.get("color")
    cell.CharColor = int(color.lstrip("#"), 16) if color else -1
    bg = style.get("bg")
    cell.CellBackColor = int(bg.lstrip("#"), 16) if bg else -1
    align = style.get("align")
    if align == "center":
        cell.HoriJustify = CENTER
    elif align == "right":
        cell.HoriJustify = RIGHT
    else:
        cell.HoriJustify = LEFT


# ---------------------------------------------------------------- 桥主体

class Bridge:
    def __init__(self, args):
        self.node = args.node.rstrip("/")
        self.interval = args.interval
        self.doc = None          # UNO 文档句柄
        self.doc_id = args.doc
        self.baseline = {}       # {sheet: {"r,c": ours}} 与服务端对齐的镜像
        self.applying = False

    # ---------- 文档与工作表管理 ----------

    def ensure_doc(self):
        if self.doc_id:
            return
        docs = http_json(self.node + "/v1/documents")
        if docs:
            self.doc_id = docs[-1]["id"]
        else:
            self.doc_id = http_json(
                self.node + "/v1/documents",
                json.dumps({"name": "新建表格", "owner": "libreoffice"}).encode("utf-8"),
                "POST")["id"]
        print("[bridge] 文档 %s" % self.doc_id)

    def ensure_sheets(self, names):
        sheets = self.doc.Sheets
        existing = [sheets.getByIndex(i).Name for i in range(sheets.Count)]
        for i, name in enumerate(names):
            if name not in existing:
                sheets.insertNewByName(name, i)
                print("[bridge] 新增工作表 %s" % name)
        for name in existing:
            if name not in names and sheets.Count > 1:
                sheets.removeByName(name)
                print("[bridge] 删除工作表 %s" % name)

    def get_sheet(self, name):
        return self.doc.Sheets.getByName(name)

    # ---------- 读 Calc → 规范镜像 ----------

    def read_calc(self):
        mirror = {}
        sheets = self.doc.Sheets
        for i in range(sheets.Count):
            sheet = sheets.getByIndex(i)
            cursor = sheet.createCursor()
            cursor.gotoEndOfUsedArea(False)
            ra = cursor.RangeAddress
            m = {}
            if ra.EndRow >= 0 and ra.EndColumn >= 0:
                formulas = sheet.getCellRangeByPosition(
                    0, 0, ra.EndColumn, ra.EndRow).getFormulaArray()
                for r, row in enumerate(formulas):
                    for c, f in enumerate(row):
                        if f in (None, ""):
                            continue
                        cell = sheet.getCellByPosition(c, r)
                        m["%d,%d" % (r, c)] = cell_to_ours(cell)
            mirror[sheet.Name] = m
        return mirror

    # ---------- 写远端 → Calc ----------

    def apply_remote(self, remote):
        self.ensure_sheets(list(remote.keys()))
        for name, cells in remote.items():
            sheet = self.get_sheet(name)
            for key, ours in cells.items():
                r, c = (int(x) for x in key.split(","))
                cell = sheet.getCellByPosition(c, r)
                if "formula" in ours:
                    cell.setFormula(ours["formula"])
                elif "value" in ours:
                    text = ours["value"]
                    try:
                        cell.setValue(float(text))
                    except ValueError:
                        cell.setString(text)
                else:
                    cell.setFormula("")
                    cell.setString("")
                if ours.get("style"):
                    apply_style(cell, ours["style"])
                else:
                    apply_style(cell, {})

    # ---------- 主循环 ----------

    def run(self):
        self.ensure_doc()
        grid = http_json("%s/v1/documents/%s/cells" % (self.node, self.doc_id))
        state.sheet = grid.get("name", "")
        self.baseline = {}
        for s in grid["sheets"]:
            m = {}
            for c in s["cells"]:
                ours = server_to_ours(c)
                if ours:
                    m["%d,%d" % (c["row"] - 1, c["col"] - 1)] = ours
            self.baseline[s["name"]] = m

        self.doc = new_calc_doc(desktop)
        self.apply_remote(self.baseline)
        print("[bridge] LibreOffice 已打开,开始双向同步(每 %.1fs 轮询)" % self.interval)

        tick = 0
        while True:
            time.sleep(self.interval)
            tick += 1
            try:
                if tick % 2 == 0:
                    self.push_local_diff()
                else:
                    self.pull_remote()
            except KeyboardInterrupt:
                print("[bridge] 退出")
                return
            except Exception as exc:
                print("[bridge] 轮询异常(将继续重试): %s" % exc)

    def push_local_diff(self):
        calc = self.read_calc()
        ops = []
        for name, cells in calc.items():
            base = self.baseline.get(name, {})
            for key, ours in cells.items():
                if canon(base.get(key)) != canon(ours):
                    r, c = (int(x) for x in key.split(","))
                    ops.append({
                        "stamp": {"ts": 0, "author": ""},
                        "type": "set_cell",
                        "sheet": name, "row": r + 1, "col": c + 1,
                        "value": ours.get("value"),
                        "formula": ours.get("formula"),
                        "style": ours.get("style"),
                    })
            for key in base:
                if key not in cells:
                    r, c = (int(x) for x in key.split(","))
                    ops.append({
                        "stamp": {"ts": 0, "author": ""},
                        "type": "set_cell",
                        "sheet": name, "row": r + 1, "col": c + 1,
                        "value": None, "formula": None, "style": None,
                    })
        # 表级增删
        for name in calc:
            if name not in self.baseline:
                ops.append({"stamp": {"ts": 0, "author": ""}, "type": "add_sheet",
                            "sheet": name, "position": None})
        for name in list(self.baseline):
            if name not in calc:
                ops.append({"stamp": {"ts": 0, "author": ""}, "type": "remove_sheet",
                            "sheet": name})
        if not ops:
            return
        # 表级 op 放最前
        ops.sort(key=lambda o: 0 if o["type"] != "set_cell" else 1)
        http_json("%s/v1/documents/%s/ops" % (self.node, self.doc_id),
                  json.dumps(ops, ensure_ascii=False).encode("utf-8"), "POST")
        print("[bridge] 本地变更上链:%d 条" % len(ops))
        for name in calc:
            self.baseline.setdefault(name, {})
            for key, ours in calc[name].items():
                self.baseline[name][key] = ours
        for name in list(self.baseline):
            if name not in calc:
                del self.baseline[name]

    def pull_remote(self):
        grid = http_json("%s/v1/documents/%s/cells" % (self.node, self.doc_id))
        remote = {}
        for s in grid["sheets"]:
            m = {}
            for c in s["cells"]:
                ours = server_to_ours(c)
                if ours:
                    m["%d,%d" % (c["row"] - 1, c["col"] - 1)] = ours
            remote[s["name"]] = m
        changed = False
        names = set(list(remote) + list(self.baseline))
        for name in names:
            base = self.baseline.get(name, {})
            rem = remote.get(name, {})
            keys = set(list(base) + list(rem))
            for key in keys:
                if canon(base.get(key)) != canon(rem.get(key)):
                    changed = True
                    break
            if changed:
                break
        if not changed:
            return
        print("[bridge] 收到远端变更,写回 Calc")
        self.applying = True
        try:
            self.apply_remote(remote)
        finally:
            self.applying = False
        self.baseline = remote


state = argparse.Namespace(sheet="")

def main():
    ap = argparse.ArgumentParser(description="MeshSExcel LibreOffice 桌面桥")
    ap.add_argument("--node", default="http://127.0.0.1:8443", help="节点 REST 地址(建议 127.0.0.1 而非 localhost,避免 IPv6 首选)")
    ap.add_argument("--doc", default=None, help="文档 id(缺省取最新/自建)")
    ap.add_argument("--soffice-path", default=None, help="soffice.exe 路径")
    ap.add_argument("--port", type=int, default=2002, help="UNO 监听端口")
    ap.add_argument("--interval", type=float, default=0.8, help="轮询间隔秒")
    args = ap.parse_args()

    soffice = find_soffice(args.soffice_path)
    _ctx, desktop = connect_uno(soffice, args.port)
    globals()["desktop"] = desktop

    bridge = Bridge(args)
    bridge.run()


if __name__ == "__main__":
    main()
