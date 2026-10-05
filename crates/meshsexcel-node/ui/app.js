/* MeshSExcel Web UI —— 无构建、无依赖的单文件前端。
 * 数据流:本地编辑 → 乐观更新 → 批量 POST /ops(服务端签名成 block 并广播)
 * 每 2.5s 轮询 /cells 对账(gossip 同步的结果由此回流)。 */
"use strict";

const $ = (id) => document.getElementById(id);
const state = {
  info: null,
  docs: [],
  docId: null,
  grid: null,          // WorkbookGrid
  sheet: null,         // 当前表名
  sel: null,           // {row, col}
  editing: false,
  pending: new Map(),  // "sheet!r,c" -> op
  saveTimer: null,
  pollTimer: null,
  peers: [],
  hlMode: false,
};

const MIN_ROWS = 40, MIN_COLS = 14, PAD_ROWS = 12, PAD_COLS = 6;

/* ---------------- 基础工具 ---------------- */

function colName(n) {
  let s = "";
  while (n > 0) { const r = (n - 1) % 26; s = String.fromCharCode(65 + r) + s; n = (n - 1 - r) / 26; }
  return s;
}
function parseCol(s) {
  let n = 0;
  for (const ch of s.toUpperCase()) {
    if (ch < "A" || ch > "Z") return null;
    n = n * 26 + (ch.charCodeAt(0) - 64);
  }
  return n || null;
}
function parseA1(a1) {
  const m = /^([A-Za-z]{1,3})(\d+)$/.exec(a1.trim());
  if (!m) return null;
  const col = parseCol(m[1]);
  const row = parseInt(m[2], 10);
  return row >= 1 && col ? { row, col } : null;
}
function a1(row, col) { return colName(col) + row; }

async function api(path, opts) {
  const res = await fetch(path, opts);
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try { msg = (await res.json()).error || msg; } catch (_) { /* 忽略 */ }
    throw new Error(msg);
  }
  return res;
}
const apiJson = (path, opts) => api(path, opts).then((r) => r.json());

let toastTimer = null;
function toast(text, kind) {
  const el = $("toast");
  el.textContent = text;
  el.className = "toast" + (kind ? " " + kind : "");
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, kind === "err" ? 4200 : 2600);
}

/* ---------------- 样式工具 ---------------- */

function parseStyle(json) {
  if (!json) return {};
  try { return JSON.parse(json) || {}; } catch (_) { return {}; }
}
function styleClass(style) {
  const cls = [];
  if (!style) return cls;
  if (style.bold) cls.push("bold");
  if (style.align === "center") cls.push("center");
  if (style.align === "right") cls.push("right");
  return cls;
}

/* ---------------- 网格数据 ---------------- */

function sheetData() {
  if (!state.grid) return null;
  return state.grid.sheets.find((s) => s.name === state.sheet) || state.grid.sheets[0] || null;
}

function cellAt(row, col) {
  const sd = sheetData();
  if (!sd) return null;
  return sd.cells.find((c) => c.row === row && c.col === col) || null;
}

function rawOf(cell) {
  if (!cell) return "";
  if (cell.formula) return cell.formula;
  return cell.value || "";
}

/* ---------------- 渲染 ---------------- */

function renderAll() {
  renderHeader();
  renderTabs();
  renderGrid();
  refreshToolbarState();
}

function renderHeader() {
  $("docName").textContent = state.grid ? state.grid.name : (state.docId ? state.docId : "未打开文档");
  document.title = (state.grid ? state.grid.name + " · " : "") + "MeshSExcel";
}

function renderTabs() {
  const host = $("sheetTabs");
  host.innerHTML = "";
  if (!state.grid) return;
  for (const s of state.grid.sheets) {
    const b = document.createElement("button");
    b.className = "sheet-tab" + (s.name === state.sheet ? " active" : "");
    b.setAttribute("role", "tab");
    b.setAttribute("aria-selected", s.name === state.sheet ? "true" : "false");
    b.textContent = s.name;
    b.onclick = () => { state.sheet = s.name; state.sel = null; renderGrid(); refreshToolbarState(); };
    host.appendChild(b);
  }
}

function extent(sd) {
  const rows = Math.max(sd ? sd.rows : 0, 0) + PAD_ROWS;
  const cols = Math.max(sd ? sd.cols : 0, 0) + PAD_COLS;
  return { rows: Math.max(rows, MIN_ROWS), cols: Math.max(cols, MIN_COLS) };
}

function renderGrid() {
  const table = $("gridTable");
  const sd = sheetData();
  table.innerHTML = "";
  if (!state.grid || !sd) {
    $("gridHost").hidden = true;
    $("emptyState").style.display = "";
    return;
  }
  $("emptyState").style.display = "none";
  $("gridHost").hidden = false;

  const { rows, cols } = extent(sd);
  const thead = document.createElement("thead");
  const hr = document.createElement("tr");
  hr.appendChild(document.createElement("th"));
  for (let c = 1; c <= cols; c++) {
    const th = document.createElement("th");
    th.textContent = colName(c);
    if (state.sel && state.sel.col === c) th.classList.add("col-sel");
    hr.appendChild(th);
  }
  thead.appendChild(hr);
  table.appendChild(thead);

  const tbody = document.createElement("tbody");
  for (let r = 1; r <= rows; r++) {
    const tr = document.createElement("tr");
    const th = document.createElement("th");
    th.textContent = r;
    if (state.sel && state.sel.row === r) th.classList.add("row-sel");
    tr.appendChild(th);
    for (let c = 1; c <= cols; c++) {
      tr.appendChild(renderCell(r, c));
    }
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
}

function renderCell(r, c) {
  const td = document.createElement("td");
  td.dataset.row = r;
  td.dataset.col = c;
  td.setAttribute("role", "gridcell");
  const cell = cellAt(r, c);
  if (cell) {
    td.textContent = cell.value || "";
    if (cell.formula) td.classList.add("formula");
    const style = cell.style || {};
    for (const cls of styleClass(style)) td.classList.add(cls);
    if (style.bg) td.style.background = style.bg;
    if (/^#(?:DIV\/0!|VALUE!|NAME\?|REF!|CIRC!|NUM!|RANGE!)/.test(cell.value || "")) td.classList.add("err");
    if (!cell.formula && cell.value !== null && cell.value !== "" && !isNaN(Number(cell.value))) td.classList.add("num");
  }
  if (state.sel && state.sel.row === r && state.sel.col === c) td.classList.add("selected");
  td.onclick = () => selectCell(r, c);
  td.ondblclick = () => { selectCell(r, c); startEdit(); };
  return td;
}

function refreshToolbarState() {
  const has = !!state.docId;
  for (const id of ["boldBtn", "hlBtn", "alignBtn", "clearBtn", "exportXlsxBtn", "exportCsvBtn", "importBtn", "snapshotBtn", "historyBtn", "addSheetBtn", "formulaInput"]) {
    $(id).disabled = !has;
  }
  $("hlBtn").classList.toggle("armed", state.hlMode);
  $("peersCount").textContent = `${state.peers.length} 节点`;
}

/* ---------------- 选择与编辑 ---------------- */

function selectCell(row, col) {
  commitEditorIfOpen(false);
  state.sel = { row, col };
  renderGrid();
  const cell = cellAt(row, col);
  $("cellRef").textContent = a1(row, col);
  $("formulaInput").value = rawOf(cell);
  $("formulaInput").focus();
}

function startEdit(initial) {
  if (!state.sel) return;
  const cell = cellAt(state.sel.row, state.sel.col);
  const input = $("formulaInput");
  input.value = initial !== undefined ? initial : rawOf(cell);
  input.focus();
  input.setSelectionRange(input.value.length, input.value.length);
}

/** 提交公式栏(或键盘输入)的当前值。 */
function commitEdit(move) {
  if (!state.sel) return;
  const { row, col } = state.sel;
  const input = $("formulaInput");
  const next = input.value;
  const cell = cellAt(row, col);
  const prev = rawOf(cell);
  if (next !== prev) {
    enqueueCellWrite(row, col, next);
  }
  if (move === "down") selectCell(Math.min(row + 1, 9999), col);
  else if (move === "right") selectCell(row, Math.min(col + 1, 9999));
  else if (move === "up") selectCell(Math.max(row - 1, 1), col);
  else if (move === "left") selectCell(row, Math.max(col - 1, 1));
  else renderGrid();
}

/** 旧式就地编辑器只在直接打字时出现;公式栏始终可用。 */
function commitEditorIfOpen(move) {
  if (state.editing) {
    state.editing = false;
    commitEdit(move);
  }
}

function enqueueCellWrite(row, col, raw) {
  if (!state.docId || !state.sheet) return;
  const isFormula = raw.startsWith("=");
  const key = `${state.sheet}!${row},${col}`;
  const prevCell = cellAt(row, col);
  const op = {
    stamp: { ts: 0, author: "" },
    type: "set_cell",
    sheet: state.sheet,
    row,
    col,
    value: isFormula ? null : (raw === "" ? null : raw),
    formula: isFormula ? raw : null,
    style: (prevCell && prevCell.style) || null,
  };
  state.pending.set(key, op);
  applyLocalOp(op);
  scheduleSave();
}

function applyLocalOp(op) {
  // 乐观更新本地网格
  if (!state.grid) return;
  let sd = state.grid.sheets.find((s) => s.name === op.sheet);
  if (!sd) {
    sd = { name: op.sheet, rows: 0, cols: 0, cells: [] };
    state.grid.sheets.push(sd);
  }
  let cell = sd.cells.find((c) => c.row === op.row && c.col === op.col);
  if (op.value === null && op.formula === null && !op.style) {
    sd.cells = sd.cells.filter((c) => !(c.row === op.row && c.col === op.col));
  } else if (cell) {
    cell.value = op.value !== null ? op.value : (op.formula ? "" : null);
    cell.formula = op.formula;
    cell.style = op.style;
  } else {
    sd.cells.push({
      row: op.row, col: op.col,
      value: op.value !== null ? op.value : (op.formula ? "" : null),
      formula: op.formula, style: op.style,
    });
  }
  sd.rows = Math.max(sd.rows, op.row);
  sd.cols = Math.max(sd.cols, op.col);
  renderGrid();
}

/* ---------------- 保存 ---------------- */

function scheduleSave() {
  $("saveStatus").textContent = "保存中…";
  $("saveStatus").className = "status-item busy";
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
    $("saveStatus").textContent = "已同步";
    $("saveStatus").className = "status-item";
    markSync();
  } catch (e) {
    $("saveStatus").textContent = "保存失败,已暂存重试";
    $("saveStatus").className = "status-item err";
    toast("保存失败:" + e.message + "(编辑已暂存,恢复联网后自动重试)", "err");
    for (const op of ops) state.pending.set(`${op.sheet}!${op.row},${op.col}`, op);
  }
}

function markSync() {
  const t = new Date();
  $("syncTime").textContent = "上次同步 " + t.toTimeString().slice(0, 8);
}

/* ---------------- 轮询对账 ---------------- */

async function poll() {
  if (!state.docId || state.pending.size > 0 || document.hidden) return;
  try {
    const grid = await apiJson(`/v1/documents/${state.docId}/cells`);
    state.grid = grid;
    if (!grid.sheets.find((s) => s.name === state.sheet)) {
      state.sheet = grid.sheets[0] ? grid.sheets[0].name : null;
    }
    renderAll();
    if (state.sel) {
      const cell = cellAt(state.sel.row, state.sel.col);
      if (document.activeElement !== $("formulaInput")) {
        $("formulaInput").value = rawOf(cell);
      }
    }
    $("saveStatus").textContent = "已同步";
    $("saveStatus").className = "status-item";
    markSync();
  } catch (_) {
    $("saveStatus").textContent = "节点连接中断";
    $("saveStatus").className = "status-item err";
  }
}

async function pollPeers() {
  try {
    state.peers = await apiJson("/v1/nodes/peers");
    refreshToolbarState();
  } catch (_) { /* 忽略 */ }
}

/* ---------------- 文档生命周期 ---------------- */

async function loadDoc(docId) {
  state.docId = docId;
  state.sel = null;
  state.pending.clear();
  location.hash = docId ? "#doc=" + docId : "";
  if (!docId) { state.grid = null; renderAll(); return; }
  const grid = await apiJson(`/v1/documents/${docId}/cells`);
  state.grid = grid;
  state.sheet = grid.sheets[0] ? grid.sheets[0].name : null;
  renderAll();
  poll();
}

async function refreshDocList() {
  state.docs = await apiJson("/v1/documents");
  if (!state.docId && state.docs.length > 0) {
    await loadDoc(state.docs[state.docs.length - 1].id);
  } else if (state.docId && !state.docs.find((d) => d.id === state.docId)) {
    await loadDoc(null);
  }
}

function openDialog({ title, desc, fields, onSubmit }) {
  const dlg = document.createElement("dialog");
  const form = document.createElement("form");
  form.innerHTML = `<div class="dialog-body">
    <h3></h3><p></p>
    ${fields.map((f, i) => f.select
      ? `<select id="dg-f${i}">${f.options.map((o) => `<option value=""></option>`).join("")}</select>`
      : `<input id="dg-f${i}" type="text" placeholder=""><p class="field-err" id="dg-e${i}"></p>`).join("")}
    <div class="dialog-actions">
      <button type="button" class="btn btn-ghost" id="dg-cancel">取消</button>
      <button type="submit" class="btn btn-primary" id="dg-ok">确定</button>
    </div></div>`;
  dlg.appendChild(form);
  dlg.querySelector("h3").textContent = title;
  dlg.querySelector("p").textContent = desc || "";
  fields.forEach((f, i) => {
    const el = dlg.querySelector(`#dg-f${i}`);
    el.placeholder = f.placeholder || "";
    if (f.select) {
      el.innerHTML = f.options.map((o) => `<option value="${o}">${o}</option>`).join("");
    }
    if (f.value) el.value = f.value;
  });
  dlg.querySelector("#dg-cancel").onclick = () => dlg.close();
  form.onsubmit = async (ev) => {
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
      dlg.remove();
    } catch (e) {
      dlg.querySelector("#dg-e0").textContent = e.message;
    }
  };
  document.body.appendChild(dlg);
  dlg.showModal();
  const first = dlg.querySelector("input, select");
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

function addSheetDialog() {
  openDialog({
    title: "新增工作表",
    fields: [{ placeholder: "表名,如:汇总", validate: (v) => (v ? "" : "表名不能为空") }],
    onSubmit: async ([name]) => {
      if (state.grid.sheets.find((s) => s.name === name)) throw new Error("表名已存在");
      await api(`/v1/documents/${state.docId}/ops`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify([{ stamp: { ts: 0, author: "" }, type: "add_sheet", sheet: name, position: null }]),
      });
      await pollForce();
      state.sheet = name;
      renderAll();
    },
  });
}

async function pollForce() {
  const grid = await apiJson(`/v1/documents/${state.docId}/cells`);
  state.grid = grid;
}

/* ---------------- 样式按钮 ---------------- */

function applyStyle(patch, clear) {
  if (!state.sel) return;
  const { row, col } = state.sel;
  const cell = cellAt(row, col);
  const base = (cell && cell.style) || {};
  const style = clear ? null : { ...base, ...patch };
  const key = `${state.sheet}!${row},${col}`;
  const op = {
    stamp: { ts: 0, author: "" },
    type: "set_cell",
    sheet: state.sheet, row, col,
    value: cell ? cell.value : null,
    formula: cell ? cell.formula : null,
    style,
  };
  state.pending.set(key, op);
  applyLocalOp(op);
  scheduleSave();
}

/* ---------------- 抽屉:历史 / 节点 / 快照 ---------------- */

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

async function showHistory() {
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

async function showPeers() {
  openDrawer("局域网节点", async (body) => {
    body.innerHTML = `<div class="drawer-empty">加载中…</div>`;
    await pollPeers();
    body.innerHTML = "";
    const self = document.createElement("div");
    self.className = "list-item";
    self.innerHTML = `<div class="li-title">本节点:${state.info ? state.info.node_id : "—"}</div>
      <div class="li-sub">${state.info ? state.info.pubkey : ""}</div>`;
    body.appendChild(self);
    if (state.peers.length === 0) {
      const empty = document.createElement("div");
      empty.className = "drawer-empty";
      empty.textContent = "尚未发现其他节点。在局域网另一台机器(或另开端口)运行 meshsexcel-node,会自动出现在这里。";
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

async function showSnapshots() {
  openDrawer("快照(备份与恢复)", async (body) => {
    body.innerHTML = `<div class="drawer-empty">加载中…</div>`;
    const snaps = await apiJson(`/v1/documents/${state.docId}/snapshots`);
    body.innerHTML = "";
    const mk = document.createElement("button");
    mk.className = "btn btn-primary";
    mk.style.marginBottom = "12px";
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
      empty.textContent = "还没有快照。快照会保存整簿内容,可随时恢复到该时点。";
      body.appendChild(empty);
      return;
    }
    for (const s of snaps) {
      const item = document.createElement("div");
      item.className = "list-item";
      item.innerHTML = `<div class="li-title"></div>
        <div class="li-sub"></div>
        <div class="li-actions"><button class="btn">恢复到此快照</button></div>`;
      item.querySelector(".li-title").textContent = s.snapshot_id.slice(0, 18) + "…";
      item.querySelector(".li-sub").textContent = `${s.description || ""} · ${(s.size / 1024).toFixed(1)} KB`;
      item.querySelector("button").onclick = () => {
        if (!confirm("恢复会用快照内容覆盖当前文档(作为新变更上链),继续?")) return;
        api(`/v1/documents/${state.docId}/restore`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ snapshot_id: s.snapshot_id }),
        }).then(() => { toast("已恢复", "ok"); return pollForce(); })
          .then(() => renderAll())
          .catch((e) => toast("恢复失败:" + e.message, "err"));
      };
      body.appendChild(item);
    }
  });
}

/* ---------------- 导入导出 ---------------- */

function download(path) {
  const a = document.createElement("a");
  a.href = path;
  a.download = "";
  document.body.appendChild(a);
  a.click();
  a.remove();
}

async function importFile(file) {
  const buf = await file.arrayBuffer();
  const isCsv = /\.csv$/i.test(file.name);
  const url = `/v1/documents/${state.docId}/import/${isCsv ? "csv" : "xlsx"}` + (isCsv ? `?sheet=${encodeURIComponent(file.name.replace(/\.csv$/i, ""))}` : "");
  await api(url, {
    method: "POST",
    headers: { "Content-Type": "application/octet-stream" },
    body: isCsv ? await file.text() : buf,
  });
  await pollForce();
  renderAll();
  toast(`已导入 ${file.name}`, "ok");
}

/* ---------------- 键盘 ---------------- */

function onGridKeydown(ev) {
  if (!state.sel) {
    if (state.docId && (ev.key === "Enter" || /^[a-zA-Z0-9=sS]$/.test(ev.key))) {
      selectCell(1, 1);
      if (ev.key.length === 1) startEdit(ev.key);
      ev.preventDefault();
    }
    return;
  }
  const { row, col } = state.sel;
  if (state.editing) return; // 编辑态交给 input 自身处理
  switch (ev.key) {
    case "ArrowUp": selectCell(Math.max(row - 1, 1), col); ev.preventDefault(); return;
    case "ArrowDown": selectCell(Math.min(row + 1, 9999), col); ev.preventDefault(); return;
    case "ArrowLeft": selectCell(row, Math.max(col - 1, 1)); ev.preventDefault(); return;
    case "ArrowRight": selectCell(row, Math.min(col + 1, 9999)); ev.preventDefault(); return;
    case "Tab": commitEdit(ev.shiftKey ? "left" : "right"); ev.preventDefault(); return;
    case "Enter": startEdit(); ev.preventDefault(); return;
    case "F2": startEdit(); ev.preventDefault(); return;
    case "Delete":
    case "Backspace": enqueueCellWrite(row, col, ""); ev.preventDefault(); return;
    case "b": case "B":
      if (ev.ctrlKey || ev.metaKey) { applyStyle({ bold: !((cellAt(row, col) || {}).style || {}).bold }); ev.preventDefault(); }
      return;
    default:
      if (ev.key.length === 1 && !ev.ctrlKey && !ev.metaKey) {
        startEdit(ev.key);
        ev.preventDefault();
      }
  }
}

/* ---------------- 启动 ---------------- */

function bindEvents() {
  $("newDocBtn").onclick = newDocDialog;
  $("emptyNewBtn").onclick = newDocDialog;
  $("addSheetBtn").onclick = addSheetDialog;
  $("historyBtn").onclick = showHistory;
  $("peersBtn").onclick = showPeers;
  $("snapshotBtn").onclick = showSnapshots;
  $("drawerClose").onclick = closeDrawer;
  $("drawerBackdrop").onclick = closeDrawer;

  $("exportXlsxBtn").onclick = () => download(`/v1/documents/${state.docId}/export/xlsx`);
  $("exportCsvBtn").onclick = () => download(`/v1/documents/${state.docId}/export/csv`);
  $("importBtn").onclick = () => $("fileInput").click();
  $("fileInput").onchange = async (ev) => {
    const file = ev.target.files[0];
    if (!file) return;
    try { await importFile(file); } catch (e) { toast("导入失败:" + e.message, "err"); }
    ev.target.value = "";
  };

  const input = $("formulaInput");
  input.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") { commitEdit("down"); ev.preventDefault(); }
    else if (ev.key === "Tab") { commitEdit(ev.shiftKey ? "left" : "right"); ev.preventDefault(); }
    else if (ev.key === "Escape") {
      const cell = cellAt(state.sel.row, state.sel.col);
      input.value = rawOf(cell);
      $("gridWrap").focus();
      ev.preventDefault();
    }
  });

  $("gridWrap").addEventListener("keydown", onGridKeydown);

  $("boldBtn").onclick = () => {
    const s = state.sel && (cellAt(state.sel.row, state.sel.col) || {}).style || {};
    applyStyle({ bold: !s.bold });
  };
  $("alignBtn").onclick = () => {
    const s = state.sel && (cellAt(state.sel.row, state.sel.col) || {}).style || {};
    applyStyle({ align: s.align === "center" ? null : "center" });
  };
  $("hlBtn").onclick = () => {
    state.hlMode = !state.hlMode;
    refreshToolbarState();
  };
  $("clearBtn").onclick = () => {
    if (state.sel) {
      enqueueCellWrite(state.sel.row, state.sel.col, "");
      applyStyle(null, true);
    }
  };
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
    if (m) {
      await loadDoc(m[1]);
    }
    await refreshDocList();
  } catch (e) {
    toast("文档加载失败:" + e.message, "err");
  }
  renderAll();
  state.pollTimer = setInterval(poll, 2500);
  setInterval(pollPeers, 5000);
  pollPeers();
}

boot();
