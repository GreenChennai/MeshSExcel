/* MeshSExcel 前端适配器:LibreOffice 血统的 Luckysheet(MIT)做表格 UI,
 * 本文件把 Luckysheet 数据模型 ↔ MeshSExcel block/LWW REST 打通。
 *
 * 数据流:用户在 Luckysheet 里编辑 → hook 触发 → diff 出变更 → 转成
 * CellOp 批量 POST /ops(服务端签名上链 + gossip);反向每 2.5s 轮询
 * /cells,发现远端变更且本地无未保存编辑时整簿重建。 */
"use strict";

const $ = (id) => document.getElementById(id);

const state = {
  info: null,
  docs: [],
  docId: null,
  grid: null,
  sheet: null,
  pending: new Map(),   // "sheet!r,c" -> 最近一次待保存 op
  saveTimer: null,
  syncTimer: null,
  luckReady: false,
  luckySheets: [],
  syncing: false,       // 正在把本地 diff 推给服务端
  peers: [],
};

/* lastSynced:{ [sheetName]: { "r,c": canonicalCell } } ——
 * 已与服务端对齐的镜像;一切 diff 都以它为基准。 */
let lastSynced = {};

/* ============================ 基础工具 ============================ */

async function api(path, opts) {
  const res = await fetch(path, opts);
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try { msg = (await res.json()).error || msg; } catch (_) { /* 忽略 */ }
    throw new Error(msg);
  }
  return res;
}
const apiJson = (p, o) => api(p, o).then((r) => r.json());

let toastTimer = null;
function toast(text, kind) {
  const el = $("toast");
  el.textContent = text;
  el.className = "toast" + (kind ? " " + kind : "");
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, kind === "err" ? 4200 : 2400);
}

/* ============================ 模型转换 ============================ */

/// 服务端 cell(grid.cells 元素)→ 规范形 { value?, formula?, style? }
function serverCellToOurs(c) {
  const ours = {};
  if (c.formula) ours.formula = c.formula;
  if (c.value !== null && c.value !== undefined && c.value !== "") ours.value = String(c.value);
  if (c.style && Object.keys(c.style).length) ours.style = c.style;
  return ours.value !== undefined || ours.formula !== undefined || ours.style ? ours : null;
}

/// Luckysheet cell v 对象 → 规范形(忽略我们不支持的属性,避免伪 diff)
function luckyCellToOurs(v) {
  if (!v || typeof v !== "object") return null;
  const ours = {};
  if (typeof v.f === "string" && v.f.length) ours.formula = "=" + v.f.replace(/^=/, "");
  if (v.v !== undefined && v.v !== null && v.v !== "") ours.value = String(v.v);
  const style = {};
  if (v.bl) style.bold = true;
  if (v.fc) style.color = v.fc;
  if (v.bg) style.bg = v.bg;
  if (v.ht === 0) style.align = "left";
  else if (v.ht === 1) style.align = "center";
  else if (v.ht === 2) style.align = "right";
  if (Object.keys(style).length) ours.style = style;
  return ours.value !== undefined || ours.formula !== undefined || ours.style ? ours : null;
}

/* 排序键序列化:避免同内容不同键序被判为 diff 产生回声 op */
const canon = (cell) => (cell ? JSON.stringify(cell, Object.keys(cell).sort()) : "");

/// WorkbookGrid → lastSynced 镜像
function gridToMirror(grid) {
  const mirror = {};
  for (const s of grid.sheets) {
    const m = {};
    for (const c of s.cells) {
      const ours = serverCellToOurs(c);
      if (ours) m[c.row - 1 + "," + (c.col - 1)] = ours;
    }
    mirror[s.name] = m;
  }
  return mirror;
}

/// WorkbookGrid → Luckysheet options.data
function gridToLucky(grid) {
  return grid.sheets.map((s, i) => ({
    name: s.name,
    color: "",
    status: s.name === state.sheet ? 1 : 0,
    order: i,
    index: String(i),
    rowCount: Math.max(84, s.rows + 20),
    columnCount: Math.max(26, s.cols + 8),
    zoomRatio: 1,
    celldata: s.cells.map((c) => {
      const v = {};
      if (c.formula) v.f = c.formula.replace(/^=/, "");
      if (c.value !== null && c.value !== undefined && c.value !== "") {
        const n = Number(c.value);
        if (!isNaN(n) && c.value.trim() !== "") {
          v.v = n;
          v.ct = { t: "n" };
        } else {
          v.v = c.value;
          v.ct = { t: "s" };
        }
      }
      if (c.style) {
        if (c.style.bold) v.bl = 1;
        if (c.style.color) v.fc = c.style.color;
        if (c.style.bg) v.bg = c.style.bg;
        if (c.style.align === "center") v.ht = 1;
        else if (c.style.align === "right") v.ht = 2;
        else if (c.style.align === "left") v.ht = 0;
      }
      return { r: c.row - 1, c: c.col - 1, v: Object.keys(v).length ? v : null };
    }),
    config: {},
  }));
}

/// 最近一次 Luckysheet 上报的表数据 → 镜像
function luckyToMirror() {
  const mirror = {};
  for (const s of state.luckySheets || []) {
    const m = {};
    for (const cd of s.celldata || []) {
      const ours = luckyCellToOurs(cd.v);
      if (ours) m[cd.r + "," + cd.c] = ours;
    }
    mirror[s.name] = m;
  }
  return mirror;
}

/* ============================ 出向:本地编辑 → ops ============================ */

let diffTimer = null;
function scheduleLocalDiff() {
  clearTimeout(diffTimer);
  diffTimer = setTimeout(pushLocalDiff, 350);
}

function pushLocalDiff() {
  if (!state.docId || state.syncing) return;
  const lucky = luckyToMirror();
  const ops = [];
  // 表级增删(Luckysheet 工作表栏的新建/删除/重命名都会在这里体现)
  for (const name of Object.keys(lucky)) {
    if (!(name in lastSynced)) {
      ops.push({ stamp: { ts: 0, author: "" }, type: "add_sheet", sheet: name, position: null });
    }
  }
  for (const name of Object.keys(lastSynced)) {
    if (!(name in lucky)) {
      ops.push({ stamp: { ts: 0, author: "" }, type: "remove_sheet", sheet: name });
    }
  }
  // 单元格级
  for (const [name, cells] of Object.entries(lucky)) {
    if (!(name in lastSynced)) continue;
    const base = lastSynced[name];
    for (const [key, ours] of Object.entries(cells)) {
      if (canon(base[key]) !== canon(ours)) {
        const [r, c] = key.split(",").map(Number);
        ops.push(cellOp(name, r, c, ours));
      }
    }
    for (const [key, ours] of Object.entries(base)) {
      if (!(key in cells)) {
        const [r, c] = key.split(",").map(Number);
        ops.push(cellOp(name, r, c, null));
      }
    }
  }
  if (ops.length === 0) return;
  // 更新镜像(以我们发出的为准),避免轮询回声触发重建
  for (const name of Object.keys(lucky)) {
    lastSynced[name] = lastSynced[name] || {};
    for (const [key, ours] of Object.entries(lucky[name])) lastSynced[name][key] = ours;
  }
  for (const name of Object.keys(lastSynced)) {
    if (!(name in lucky)) delete lastSynced[name];
  }
  enqueueOps(ops);
}

function cellOp(sheet, r, c, ours) {
  return {
    stamp: { ts: 0, author: "" },
    type: "set_cell",
    sheet,
    row: r + 1,
    col: c + 1,
    value: ours && ours.value !== undefined ? ours.value : null,
    formula: ours && ours.formula !== undefined ? ours.formula : null,
    style: ours && ours.style ? ours.style : null,
  };
}

function enqueueOps(ops) {
  for (const op of ops) {
    state.pending.set(`${op.sheet}!${op.row},${op.col}`, op);
  }
  setSaveState("保存中…", false);
  clearTimeout(state.saveTimer);
  state.saveTimer = setTimeout(flushSave, 350);
}

async function flushSave() {
  if (state.pending.size === 0) return;
  const ops = [...state.pending.values()];
  state.pending.clear();
  try {
    await api(`/v1/documents/${state.docId}/ops`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(ops),
    });
    setSaveState("已保存", false);
    markSync();
  } catch (e) {
    setSaveState("保存失败,重试中", true);
    toast("保存失败:" + e.message + "(编辑已暂存)", "err");
    for (const op of ops) state.pending.set(`${op.sheet}!${op.row},${op.col}`, op);
  }
}

function setSaveState(text, err) {
  const el = $("saveState");
  el.textContent = text;
  el.classList.toggle("err", !!err);
}
function markSync() {
  const t = new Date();
  const el = $("syncState");
  el.textContent = "已同步 " + t.toTimeString().slice(0, 8);
  el.classList.remove("err");
}

/* ============================ 入向:远端 → Luckysheet ============================ */

async function pollRemote() {
  if (!state.docId || state.pending.size > 0 || state.syncing || document.hidden) return;
  try {
    const grid = await apiJson(`/v1/documents/${state.docId}/cells`);
    const remote = gridToMirror(grid);
    if (mirrorDiffers(lastSynced, remote)) {
      await rebuildFromRemote(grid, remote);
    }
    setSaveState(( $("saveState").textContent || "").replace("保存失败,重试中", ""), false);
    markSync();
  } catch (_) {
    const el = $("syncState");
    el.textContent = "连接中断";
    el.classList.add("err");
  }
}

function mirrorDiffers(a, b) {
  const ka = Object.keys(a), kb = Object.keys(b);
  if (ka.length !== kb.length) return true;
  for (const k of ka) {
    if (!(k in b)) return true;
    const ma = a[k], mb = b[k];
    const keys = new Set([...Object.keys(ma), ...Object.keys(mb)]);
    for (const ck of keys) {
      if (canon(ma[ck]) !== canon(mb[ck])) return true;
    }
  }
  return false;
}

async function rebuildFromRemote(grid, remote) {
  state.syncing = true;
  try {
    state.grid = grid;
    state.sheet = grid.sheets.find((s) => s.name === state.sheet)
      ? state.sheet
      : (grid.sheets[0] ? grid.sheets[0].name : null);
    lastSynced = remote;
    createLucky(grid);
    await new Promise((r) => setTimeout(r, 120));
  } finally {
    state.syncing = false;
  }
}

/* ============================ Luckysheet 装载 ============================ */

function createLucky(grid) {
  state.luckReady = false;
  sendToSheet({ type: "load", title: grid.name, data: gridToLucky(grid) });
}

function sendToSheet(msg) {
  $("luckyFrame").contentWindow.postMessage(msg, "*");
}

function onSheetMessage(ev) {
  const msg = ev.data || {};
  if (ev.source !== $("luckyFrame").contentWindow) return;
  if (msg.type === "ready") {
    state.luckReady = true;
  } else if (msg.type === "changed") {
    state.luckySheets = msg.sheets || [];
    scheduleLocalDiff();
  } else if (msg.type === "create-error") {
    toast("表格组件加载失败:" + msg.message, "err");
  }
}

/* ============================ 文档生命周期 ============================ */

async function loadDoc(docId) {
  state.docId = docId;
  location.hash = docId ? "#doc=" + docId : "";
  lastSynced = {};
  if (!docId) {
    state.grid = null;
    $("luckyFrame").hidden = true;
    $("emptyState").style.display = "";
    refreshDocUi();
    return;
  }
  const grid = await apiJson(`/v1/documents/${docId}/cells`);
  state.grid = grid;
  state.sheet = grid.sheets[0] ? grid.sheets[0].name : null;
  lastSynced = gridToMirror(grid);
  $("emptyState").style.display = "none";
  $("luckyFrame").hidden = false;
  createLucky(grid);
  refreshDocUi();
  markSync();
}

function refreshDocUi() {
  const sel = $("docSel");
  sel.innerHTML = "";
  for (const d of state.docs) {
    const opt = document.createElement("option");
    opt.value = d.id;
    opt.textContent = d.name;
    if (d.id === state.docId) opt.selected = true;
    sel.appendChild(opt);
  }
  sel.disabled = state.docs.length === 0;
  const has = !!state.docId;
  for (const id of ["historyBtn", "snapshotBtn"]) $(id).disabled = !has;
}

async function refreshDocList() {
  state.docs = await apiJson("/v1/documents");
  if (!state.docId && state.docs.length > 0) {
    await loadDoc(state.docs[state.docs.length - 1].id);
  } else if (state.docId && !state.docs.find((d) => d.id === state.docId)) {
    await loadDoc(null);
  }
  refreshDocUi();
}

async function pollPeers() {
  try {
    state.peers = await apiJson("/v1/nodes/peers");
    $("peersCount").textContent = `${state.peers.length} 节点`;
  } catch (_) { /* 忽略 */ }
}

/* ============================ 抽屉(历史/节点/快照) ============================ */

function openDrawer(title, render) {
  $("drawerTitle").textContent = title;
  const body = $("drawerBody");
  body.innerHTML = "";
  render(body);
  $("drawer").hidden = false;
  $("drawerBackdrop").hidden = false;
  $("drawerClose").focus();
}
function closeDrawer() {
  $("drawer").hidden = true;
  $("drawerBackdrop").hidden = true;
}

function showHistory() {
  openDrawer("审计历史(block 链)", async (body) => {
    body.innerHTML = `<div class="drawer-empty">加载中…</div>`;
    const blocks = await apiJson(`/v1/documents/${state.docId}/blocks`);
    body.innerHTML = "";
    if (blocks.length === 0) {
      body.innerHTML = `<div class="drawer-empty">还没有任何变更记录</div>`;
      return;
    }
    for (const b of blocks.slice().reverse()) {
      const item = document.createElement("div");
      item.className = "list-item";
      const time = new Date(b.header.timestamp).toLocaleString("zh-CN");
      const ops = (b.payload.operations || []).join("; ");
      item.innerHTML = `
        <div class="li-title"></div>
        <div class="li-sub"></div>
        <div class="li-sub" style="font-family:var(--font)"></div>`;
      item.querySelector(".li-title").textContent = `${b.header.author} · ${time}`;
      item.querySelectorAll(".li-sub")[0].textContent = "hash " + (b.merkle_root || "").slice(0, 16) + "…";
      item.querySelectorAll(".li-sub")[1].textContent = ops || "(空操作)";
      body.appendChild(item);
    }
  });
}

function showPeers() {
  openDrawer("局域网节点", async (body) => {
    body.innerHTML = `<div class="drawer-empty">加载中…</div>`;
    await pollPeers();
    body.innerHTML = "";
    const self = document.createElement("div");
    self.className = "list-item";
    self.innerHTML = `<div class="li-title"></div><div class="li-sub"></div>`;
    self.querySelector(".li-title").textContent = `本节点:${state.info ? state.info.node_id : "—"}`;
    self.querySelector(".li-sub").textContent = state.info ? state.info.pubkey : "";
    body.appendChild(self);
    if (state.peers.length === 0) {
      const empty = document.createElement("div");
      empty.className = "drawer-empty";
      empty.textContent = "尚未发现其他节点。局域网内其他机器运行 meshsexcel-node 后会自动出现在这里。";
      body.appendChild(empty);
      return;
    }
    for (const p of state.peers) {
      const item = document.createElement("div");
      item.className = "list-item";
      item.innerHTML = `<div class="li-title"></div><div class="li-sub"></div><div class="li-sub"></div>`;
      item.querySelector(".li-title").textContent = p.id.slice(0, 20) + "…";
      item.querySelectorAll(".li-sub")[0].textContent = p.addr || "(地址未知)";
      item.querySelectorAll(".li-sub")[1].textContent = p.last_seen ? "最近活跃 " + p.last_seen : "";
      body.appendChild(item);
    }
  });
}

function showSnapshots() {
  openDrawer("快照(备份与恢复)", async (body) => {
    body.innerHTML = `<div class="drawer-empty">加载中…</div>`;
    const snaps = await apiJson(`/v1/documents/${state.docId}/snapshots`);
    body.innerHTML = "";
    const mk = document.createElement("button");
    mk.className = "pri-btn";
    mk.textContent = "建立当前快照";
    mk.onclick = async () => {
      await api(`/v1/documents/${state.docId}/snapshots`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ description: new Date().toLocaleString("zh-CN") }),
      });
      toast("快照已建立", "ok");
      showSnapshots();
    };
    body.appendChild(mk);
    if (snaps.length === 0) {
      const empty = document.createElement("div");
      empty.className = "drawer-empty";
      empty.textContent = "还没有快照。快照保存整簿内容,可随时恢复。";
      body.appendChild(empty);
      return;
    }
    for (const s of snaps) {
      const item = document.createElement("div");
      item.className = "list-item";
      item.innerHTML = `<div class="li-title"></div>
        <div class="li-sub"></div>
        <div class="li-actions"><button class="danger">恢复到此快照</button></div>`;
      item.querySelector(".li-title").textContent = s.snapshot_id.slice(0, 18) + "…";
      item.querySelector(".li-sub").textContent = `${s.description || ""} · ${(s.size / 1024).toFixed(1)} KB`;
      item.querySelector("button").onclick = () => {
        if (!confirm("恢复会用快照内容覆盖当前文档(作为新变更上链),继续?")) return;
        api(`/v1/documents/${state.docId}/restore`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ snapshot_id: s.snapshot_id }),
        }).then(() => { toast("已恢复", "ok"); return loadDoc(state.docId); })
          .catch((e) => toast("恢复失败:" + e.message, "err"));
      };
      body.appendChild(item);
    }
  });
}

/* ============================ 对话框 ============================ */

function openDialog({ title, desc, fields, onSubmit }) {
  const dlg = $("dlg");
  dlg.innerHTML = `<form class="dialog-body">
    <h3></h3><p></p>
    ${fields.map((f, i) => `<input id="dg-f${i}" type="text" placeholder=""><p class="field-err" id="dg-e${i}"></p>`).join("")}
    <div class="dialog-actions">
      <button type="button" class="ghost" id="dg-cancel">取消</button>
      <button type="submit" class="btn-primary" id="dg-ok">确定</button>
    </div></form>`;
  dlg.querySelector("h3").textContent = title;
  dlg.querySelector("p").textContent = desc || "";
  fields.forEach((f, i) => {
    const el = dlg.querySelector(`#dg-f${i}`);
    el.placeholder = f.placeholder || "";
    if (f.value) el.value = f.value;
  });
  dlg.querySelector("#dg-cancel").onclick = () => dlg.close();
  dlg.querySelector("#dg-cancel").classList.add("ghost");
  dlg.querySelector("form").onsubmit = async (ev) => {
    ev.preventDefault();
    const values = fields.map((_, i) => dlg.querySelector(`#dg-f${i}`).value.trim());
    for (let i = 0; i < fields.length; i++) {
      const err = fields[i].validate ? fields[i].validate(values[i]) : "";
      dlg.querySelector(`#dg-e${i}`).textContent = err || "";
      if (err) return;
    }
    try {
      await onSubmit(values);
      dlg.close();
    } catch (e) {
      dlg.querySelector("#dg-e0").textContent = e.message;
    }
  };
  dlg.showModal();
  const first = dlg.querySelector("input");
  if (first) first.focus();
}

function newDocDialog() {
  openDialog({
    title: "新建文档",
    desc: "文档会通过 gossip 自动同步到局域网内的其他节点。",
    fields: [
      { placeholder: "文档名称,如:三月生产台账", validate: (v) => (v ? "" : "名称不能为空") },
      { placeholder: "负责人(可选,默认本节点)" },
    ],
    onSubmit: async ([name, owner]) => {
      const doc = await apiJson("/v1/documents", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name, owner }),
      });
      await refreshDocList();
      await loadDoc(doc.id);
      toast("文档已创建", "ok");
    },
  });
}

/* ============================ 启动 ============================ */

function bindEvents() {
  window.addEventListener("message", onSheetMessage);
  $("docSel").onchange = (ev) => loadDoc(ev.target.value || null);
  $("newDocBtn").onclick = newDocDialog;
  $("emptyNewBtn").onclick = newDocDialog;
  $("historyBtn").onclick = showHistory;
  $("peersBtn").onclick = showPeers;
  $("snapshotBtn").onclick = showSnapshots;
  $("drawerClose").onclick = closeDrawer;
  $("drawerBackdrop").onclick = closeDrawer;
  document.addEventListener("keydown", (ev) => { if (ev.key === "Escape") closeDrawer(); });
}

async function boot() {
  bindEvents();
  try {
    state.info = await apiJson("/v1/node/info");
    $("nodeIdText").textContent = state.info.node_id;
  } catch (e) {
    toast("节点信息获取失败:" + e.message, "err");
  }
  const m = /^#doc=(.+)$/.exec(location.hash);
  try {
    if (m) await loadDoc(m[1]);
    await refreshDocList();
  } catch (e) {
    toast("文档加载失败:" + e.message, "err");
  }
  markSync();
  state.syncTimer = setInterval(pollRemote, 2500);
  setInterval(pollPeers, 5000);
  pollPeers();
}

boot();
