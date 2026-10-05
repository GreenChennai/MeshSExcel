/* MeshSExcel UI —— LibreOffice Calc / WPS 风格前端(无构建、无依赖)。
 *
 * 数据流:本地编辑 → 乐观更新 → 批量 POST /ops(服务端签名成 block 并广播)
 * 每 2.5s 轮询 /cells 对账(gossip 同步的结果由此回流)。
 * 区域选择 / 单元格内编辑 / 右键菜单 / 列宽拖拽 / 内部复制粘贴 均在此实现。 */
"use strict";

const $ = (id) => document.getElementById(id);

/* ============================ 全局状态 ============================ */

const state = {
  info: null,
  docs: [],
  docId: null,
  grid: null,            // WorkbookGrid
  sheet: null,
  sel: null,             // {ar, ac, er, ec} 锚点 + 扩展端(行列均 1-based)
  editing: false,        // 单元格内编辑中
  editingCell: null,     // {row, col}
  pending: new Map(),    // "sheet!r,c" -> op
  saveTimer: null,
  clipboard: null,       // {w, h, cells: [[{value, formula, style}]]}
  colW: {},              // col index -> px(会话级)
  zoom: 1,
  showFormula: false,
  peers: [],
  fontColor: "#e03131",
  fillColor: "#fff3bf",
};

const MIN_ROWS = 60, MIN_COLS = 20, PAD_ROWS = 14, PAD_COLS = 6;
const FONT_COLORS = ["#e03131", "#e8590c", "#f08c00", "#2b8a3e", "#1971c2", "#6741d9", "#c2255c", "#1f2328"];
const FILL_COLORS = ["#fff3bf", "#ffe3e3", "#d3f9d8", "#d0ebff", "#e5dbff", "#ffdeeb", "#e9ecef", "#ffffff"];

/* ============================ 基础工具 ============================ */

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
function rangeText(sel) {
  const a = a1(sel.ar, sel.ac), b = a1(sel.er, sel.ec);
  return a === b ? a : `${a}:${b}`;
}

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
  toastTimer = setTimeout(() => { el.hidden = true; }, kind === "err" ? 4200 : 2400);
}

/* ============================ 网格数据 ============================ */

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
function isNumericDisplay(cell) {
  if (!cell || cell.formula) return false;
  const v = cell.value;
  return v !== null && v !== undefined && v !== "" && !isNaN(Number(v));
}
function selRect() {
  const s = state.sel;
  if (!s) return null;
  return {
    r1: Math.min(s.ar, s.er), c1: Math.min(s.ac, s.ec),
    r2: Math.max(s.ar, s.er), c2: Math.max(s.ac, s.ec),
  };
}

/* ============================ 渲染 ============================ */

const tdIndex = new Map();   // "r,c" -> td(选中类增量更新用)
const colHeadIndex = new Map();
let rowHeadEls = [];

function renderAll() {
  renderChrome();
  renderTabs();
  renderGrid();
  updateSelectionUI();
}

function renderChrome() {
  $("docName").textContent = state.grid ? state.grid.name : (state.docId || "未打开文档");
  document.title = (state.grid ? state.grid.name + " - " : "") + "MeshSExcel";
  const has = !!state.docId;
  for (const id of ["formulaInput", "nameBox"]) $(id).disabled = !has;
  $("peersCount").textContent = `${state.peers.length} 节点`;
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
    b.onclick = () => { state.sheet = s.name; state.sel = null; renderGrid(); updateSelectionUI(); };
    b.oncontextmenu = (ev) => { ev.preventDefault(); sheetTabMenu(ev, s.name); };
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
  tdIndex.clear();
  colHeadIndex.clear();
  rowHeadEls = [];
  if (!state.grid || !sd) {
    $("gridHost").hidden = true;
    $("emptyState").style.display = "";
    return;
  }
  $("emptyState").style.display = "none";
  $("gridHost").hidden = false;

  const { rows, cols } = extent(sd);

  // 列宽(colgroup,支持拖拽调宽)
  const colgroup = document.createElement("colgroup");
  const rc = document.createElement("col");
  rc.className = "rowhead-col";
  colgroup.appendChild(rc);
  for (let c = 1; c <= cols; c++) {
    const col = document.createElement("col");
    if (state.colW[c]) col.style.width = state.colW[c] + "px";
    colgroup.appendChild(col);
  }
  table.appendChild(colgroup);

  // 表头
  const thead = document.createElement("thead");
  const hr = document.createElement("tr");
  const corner = document.createElement("th");
  corner.className = "corner";
  corner.setAttribute("aria-label", "全选");
  hr.appendChild(corner);
  for (let c = 1; c <= cols; c++) {
    const th = document.createElement("th");
    th.textContent = colName(c);
    th.dataset.col = c;
    const rz = document.createElement("div");
    rz.className = "col-resizer";
    rz.dataset.col = c;
    th.appendChild(rz);
    colHeadIndex.set(c, th);
    hr.appendChild(th);
  }
  thead.appendChild(hr);
  table.appendChild(thead);

  // 表体
  const tbody = document.createElement("tbody");
  for (let r = 1; r <= rows; r++) {
    const tr = document.createElement("tr");
    const th = document.createElement("th");
    th.textContent = r;
    rowHeadEls.push(th);
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
  tdIndex.set(r + "," + c, td);
  const cell = cellAt(r, c);
  if (cell) {
    if (state.showFormula && cell.formula) td.textContent = cell.formula;
    else td.textContent = cell.value || "";
    if (cell.formula) td.classList.add("formula");
    const st = cell.style || {};
    if (st.bold) td.classList.add("bold");
    if (st.align === "center") td.classList.add("center");
    if (st.align === "right") td.classList.add("right");
    if (st.bg) td.style.background = st.bg;
    if (st.color) td.style.color = st.color;
    if (/^#(?:DIV\/0!|VALUE!|NAME\?|REF!|CIRC!|NUM!|RANGE!)/.test(cell.value || "")) td.classList.add("err");
    else if (isNumericDisplay(cell)) td.classList.add("right");
  }
  td.addEventListener("mousedown", onCellMouseDown);
  td.addEventListener("dblclick", () => { setSelection(r, c); openEditor(); });
  td.addEventListener("contextmenu", (ev) => { ev.preventDefault(); cellMenu(ev, r, c); });
  return td;
}

/* ============================ 选择 ============================ */

function setSelection(ar, ac, er, ec) {
  state.sel = { ar, ac, er: er ?? ar, ec: ec ?? ac };
  updateSelectionUI();
}

function updateSelectionUI() {
  const rect = selRect();
  // 清掉旧类
  for (const td of tdIndex.values()) td.classList.remove("sel-anchor", "sel-range");
  for (const th of colHeadIndex.values()) th.classList.remove("col-sel");
  for (const th of rowHeadEls) th.classList.remove("row-sel");
  if (!rect || !state.grid) {
    $("nameBox").value = "";
    $("selStats").textContent = "";
    refreshRibbonState();
    return;
  }
  for (let r = rect.r1; r <= rect.r2; r++) {
    for (let c = rect.c1; c <= rect.c2; c++) {
      const td = tdIndex.get(r + "," + c);
      if (td) {
        td.classList.add("sel-range");
        if (r === state.sel.ar && c === state.sel.ac) td.classList.add("sel-anchor");
      }
    }
  }
  for (let c = rect.c1; c <= rect.c2; c++) colHeadIndex.get(c)?.classList.add("col-sel");
  for (let r = rect.r1; r <= rect.r2; r++) {
    const th = rowHeadEls[r - 1];
    if (th) th.classList.add("row-sel");
  }
  $("nameBox").value = rangeText(state.sel);
  updateStats();
  syncFormulaBar();
  refreshRibbonState();
}

function updateStats() {
  const rect = selRect();
  if (!rect) { $("selStats").textContent = ""; return; }
  let count = 0, sum = 0;
  for (let r = rect.r1; r <= rect.r2; r++) {
    for (let c = rect.c1; c <= rect.c2; c++) {
      const cell = cellAt(r, c);
      if (!cell || cell.formula) continue;
      const n = Number(cell.value);
      if (cell.value !== null && cell.value !== "" && !isNaN(n)) { count++; sum += n; }
    }
  }
  if (count > 0) {
    const avg = sum / count;
    const fmt = (x) => Number(x.toFixed(4)).toString();
    $("selStats").textContent = `计数:${count}  求和:${fmt(sum)}  平均:${fmt(avg)}`;
  } else {
    $("selStats").textContent = "";
  }
}

function onCellMouseDown(ev) {
  const r = +ev.currentTarget.dataset.row;
  const c = +ev.currentTarget.dataset.col;
  closeEditor(false);
  if (ev.shiftKey && state.sel) {
    setSelection(state.sel.ar, state.sel.ac, r, c);
  } else {
    setSelection(r, c);
    // 拖拽扩展选择
    const onMove = (e2) => {
      const td = e2.target.closest("td[data-row]");
      if (td) setSelection(state.sel.ar, state.sel.ac, +td.dataset.row, +td.dataset.col);
    };
    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }
  ev.preventDefault();
  $("gridWrap").focus();
}

function scrollSelIntoView() {
  if (!state.sel) return;
  const td = tdIndex.get(state.sel.ar + "," + state.sel.ac);
  td?.scrollIntoView({ block: "nearest", inline: "nearest" });
}

/* ============================ 编辑 ============================ */

function anchorCell() {
  if (!state.sel) return null;
  return cellAt(state.sel.ar, state.sel.ac);
}

function syncFormulaBar() {
  if (document.activeElement === $("formulaInput")) return;
  $("formulaInput").value = rawOf(anchorCell());
}

function openEditor(initial) {
  if (!state.sel) return;
  const { ar, ac } = state.sel;
  const cell = cellAt(ar, ac);
  const ed = $("cellEditor");
  const td = tdIndex.get(ar + "," + ac);
  if (!td) return;
  state.editing = true;
  state.editingCell = { row: ar, col: ac };
  const hostRect = $("gridHost").getBoundingClientRect();
  const tdRect = td.getBoundingClientRect();
  ed.style.left = (tdRect.left - hostRect.left) + "px";
  ed.style.top = (tdRect.top - hostRect.top) + "px";
  ed.style.width = Math.max(tdRect.width, 80) + "px";
  ed.style.height = tdRect.height + "px";
  ed.hidden = false;
  ed.value = initial !== undefined ? initial : rawOf(cell);
  $("formulaInput").value = ed.value;
  setMode("输入");
  ed.focus();
  ed.setSelectionRange(ed.value.length, ed.value.length);
}

function closeEditor(commit, move) {
  const ed = $("cellEditor");
  if (!state.editing) { if (!commit) return; }
  if (state.editing && commit && state.editingCell) {
    const { row, col } = state.editingCell;
    const next = ed.value;
    if (next !== rawOf(cellAt(row, col))) enqueueCellWrite(row, col, next);
  }
  state.editing = false;
  state.editingCell = null;
  ed.hidden = true;
  setMode("就绪");
  if (move === "down") moveSel(1, 0);
  else if (move === "right") moveSel(0, 1);
  else if (move === "up") moveSel(-1, 0);
  else if (move === "left") moveSel(0, -1);
  else { syncFormulaBar(); }
}

function setMode(m) {
  const el = $("modeCell");
  el.textContent = m;
  el.classList.toggle("editing", m === "输入");
}

function moveSel(dr, dc, extend) {
  if (!state.sel) { setSelection(1, 1); return; }
  if (extend) {
    setSelection(state.sel.ar, state.sel.ac,
      Math.max(1, state.sel.er + dr), Math.max(1, state.sel.ec + dc));
  } else {
    setSelection(
      Math.max(1, state.sel.er + dr),
      Math.max(1, state.sel.ec + dc));
  }
  scrollSelIntoView();
}

/* ============================ 写入 / 保存 ============================ */

function enqueueCellWrite(row, col, raw, styleOverride) {
  if (!state.docId || !state.sheet) return;
  const isFormula = typeof raw === "string" && raw.startsWith("=");
  const key = `${state.sheet}!${row},${col}`;
  const prev = cellAt(row, col);
  const op = {
    stamp: { ts: 0, author: "" },
    type: "set_cell",
    sheet: state.sheet,
    row, col,
    value: isFormula ? null : (raw === "" ? null : raw),
    formula: isFormula ? raw : null,
    style: styleOverride !== undefined ? styleOverride : ((prev && prev.style) || null),
  };
  state.pending.set(key, op);
  applyLocalOp(op);
  scheduleSave();
}

function applyLocalOp(op) {
  if (!state.grid) return;
  let sd = state.grid.sheets.find((s) => s.name === op.sheet);
  if (!sd) {
    sd = { name: op.sheet, rows: 0, cols: 0, cells: [] };
    state.grid.sheets.push(sd);
  }
  let cell = sd.cells.find((c) => c.row === op.row && c.col === op.col);
  const empty = op.value === null && op.formula === null && !op.style;
  if (empty) {
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
  updateSelectionUI();
}

function scheduleSave() {
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
    toast("保存失败:" + e.message + "(编辑已暂存,恢复后自动重试)", "err");
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

/* ============================ 轮询对账 ============================ */

async function poll() {
  if (!state.docId || state.pending.size > 0 || state.editing || document.hidden) return;
  try {
    const grid = await apiJson(`/v1/documents/${state.docId}/cells`);
    state.grid = grid;
    if (!grid.sheets.find((s) => s.name === state.sheet)) {
      state.sheet = grid.sheets[0] ? grid.sheets[0].name : null;
    }
    renderGrid();
    updateSelectionUI();
    setSaveState("", false);
    markSync();
  } catch (_) {
    const el = $("syncState");
    el.textContent = "连接中断";
    el.classList.add("err");
  }
}

async function pollPeers() {
  try {
    state.peers = await apiJson("/v1/nodes/peers");
    renderChrome();
  } catch (_) { /* 忽略 */ }
}
async function pollForce() {
  const grid = await apiJson(`/v1/documents/${state.docId}/cells`);
  state.grid = grid;
}

/* ============================ 样式 ============================ */

function forSelectionCells(fn) {
  const rect = selRect();
  if (!rect) return;
  for (let r = rect.r1; r <= rect.r2; r++) {
    for (let c = rect.c1; c <= rect.c2; c++) {
      fn(r, c, cellAt(r, c));
    }
  }
}

function applyStyleToSelection(patch, mode) {
  if (!state.sel) return;
  // 以锚点格的样式为基准做切换
  const base = (anchorCell()?.style) || {};
  forSelectionCells((r, c, cell) => {
    const cur = (cell?.style) || {};
    let next;
    if (mode === "clear") next = null;
    else {
      next = { ...cur };
      if (patch.bold !== undefined) next.bold = !base.bold;
      if (patch.color !== undefined) next.color = patch.color || null;
      if (patch.bg !== undefined) next.bg = patch.bg || null;
      if (patch.align !== undefined) next.align = patch.align || null;
      if (next.bold === false && !next.color && !next.bg && !next.align) next = null;
    }
    enqueueCellWrite(r, c, rawOf(cell), next);
  });
  scheduleSave();
}

function updateBoldButton() {
  const on = !!(anchorCell()?.style?.bold);
  $("boldBtn").classList.toggle("on", on);
}

/* ============================ 颜色浮层 ============================ */

function showColorPop(anchorBtn, colors, onPick, allowNone) {
  const pop = $("colorPop");
  pop.innerHTML = "";
  const title = document.createElement("div");
  title.className = "cp-title";
  title.textContent = allowNone ? "选择颜色(可选“无”)" : "选择颜色";
  pop.appendChild(title);
  const grid = document.createElement("div");
  grid.className = "color-grid";
  if (allowNone) {
    const none = document.createElement("button");
    none.className = "swatch none";
    none.title = "无填充";
    none.onclick = () => { onPick(null); hideColorPop(); };
    grid.appendChild(none);
  }
  for (const c of colors) {
    const b = document.createElement("button");
    b.className = "swatch";
    b.style.background = c;
    b.title = c;
    b.onclick = () => { onPick(c); hideColorPop(); };
    grid.appendChild(b);
  }
  pop.appendChild(grid);
  const rect = anchorBtn.getBoundingClientRect();
  pop.hidden = false;
  const pw = pop.offsetWidth, ph = pop.offsetHeight;
  pop.style.left = Math.min(rect.left, innerWidth - pw - 8) + "px";
  pop.style.top = (rect.bottom + 4 + ph > innerHeight ? rect.top - ph - 4 : rect.bottom + 4) + "px";
}
function hideColorPop() { $("colorPop").hidden = true; }

/* ============================ 右键菜单 ============================ */

function showCtxMenu(ev, items) {
  const menu = $("ctxMenu");
  menu.innerHTML = "";
  for (const it of items) {
    if (it === "-") {
      const sep = document.createElement("div");
      sep.className = "ctx-sep";
      menu.appendChild(sep);
      continue;
    }
    const b = document.createElement("button");
    b.className = "ctx-item";
    b.setAttribute("role", "menuitem");
    b.innerHTML = `<span></span>${it.key ? `<span class="ks">${it.key}</span>` : ""}`;
    b.firstChild.textContent = it.label;
    b.disabled = !!it.disabled;
    b.onclick = () => { hideCtxMenu(); it.action(); };
    menu.appendChild(b);
  }
  menu.hidden = false;
  const mw = menu.offsetWidth, mh = menu.offsetHeight;
  menu.style.left = Math.min(ev.clientX, innerWidth - mw - 6) + "px";
  menu.style.top = Math.min(ev.clientY, innerHeight - mh - 6) + "px";
}
function hideCtxMenu() { $("ctxMenu").hidden = true; }

function cellMenu(ev, r, c) {
  if (!state.sel || selRect().r1 > r || selRect().r2 < r || selRect().c1 > c || selRect().c2 < c) {
    setSelection(r, c);
  }
  showCtxMenu(ev, [
    { label: "复制", key: "Ctrl+C", action: copySelection },
    { label: "粘贴", key: "Ctrl+V", action: pasteClipboard, disabled: !state.clipboard },
    "-",
    { label: "加粗", key: "Ctrl+B", action: () => applyStyleToSelection({ bold: true }) },
    { label: "清除内容", key: "Del", action: clearContents },
    { label: "清除格式", action: () => applyStyleToSelection({}, "clear") },
  ]);
}

function sheetTabMenu(ev, name) {
  showCtxMenu(ev, [
    { label: "新建工作表", action: addSheetDialog },
    { label: `删除「${name}」`, action: () => removeSheet(name), disabled: state.grid.sheets.length <= 1 },
  ]);
}

/* ============================ 复制 / 粘贴 ============================ */

function copySelection() {
  const rect = selRect();
  if (!rect) return;
  const cells = [];
  for (let r = rect.r1; r <= rect.r2; r++) {
    const row = [];
    for (let c = rect.c1; c <= rect.c2; c++) {
      const cell = cellAt(r, c);
      row.push(cell ? { value: cell.value, formula: cell.formula, style: cell.style } : null);
    }
    cells.push(row);
  }
  state.clipboard = { w: rect.c2 - rect.c1 + 1, h: rect.r2 - rect.r1 + 1, cells };
  toast(`已复制 ${state.clipboard.h}×${state.clipboard.w}`);
}

function pasteClipboard() {
  if (!state.clipboard || !state.sel) return;
  const { w, h, cells } = state.clipboard;
  const { ar, ac } = state.sel;
  for (let dr = 0; dr < h; dr++) {
    for (let dc = 0; dc < w; dc++) {
      const src = cells[dr][dc];
      const r = ar + dr, c = ac + dc;
      const key = `${state.sheet}!${r},${c}`;
      if (!src) {
        state.pending.set(key, {
          stamp: { ts: 0, author: "" }, type: "set_cell",
          sheet: state.sheet, row: r, col: c, value: null, formula: null, style: null,
        });
        applyLocalOp(state.pending.get(key));
      } else {
        enqueueCellWrite(r, c, src.formula ?? src.value ?? "", src.style ?? null);
      }
    }
  }
  scheduleSave();
}

function clearContents() {
  const rect = selRect();
  if (!rect) return;
  forSelectionCells((r, c, cell) => {
    if (cell) enqueueCellWrite(r, c, "");
  });
  scheduleSave();
}

/* ============================ 自动求和 / 函数 ============================ */

function insertFunction(fn) {
  if (!state.sel) return;
  const { ar, ac } = state.sel;
  // 找当前列上方连续非空区
  let top = ar - 1;
  while (top >= 1 && ar - top < 100) {
    const cell = cellAt(top, ac);
    if (!cell || (cell.value === null && !cell.formula)) break;
    top--;
  }
  top++;
  const expr = top < ar ? `=${fn}(${a1(top, ac)}:${a1(ar - 1, ac)})` : `=${fn}()`;
  setSelection(ar, ac);
  openEditor(expr);
}

/* ============================ 列宽拖拽 ============================ */

function bindColumnResize() {
  $("gridTable").addEventListener("mousedown", (ev) => {
    const rz = ev.target.closest(".col-resizer");
    if (!rz) return;
    ev.preventDefault();
    ev.stopPropagation();
    const col = +rz.dataset.col;
    rz.classList.add("dragging");
    const startX = ev.clientX;
    const colEl = $("gridTable").querySelectorAll("colgroup col")[col];
    const startW = colEl.getBoundingClientRect().width;
    const onMove = (e2) => {
      const w = Math.max(40, Math.min(400, Math.round(startW + e2.clientX - startX)));
      state.colW[col] = w;
      colEl.style.width = w + "px";
    };
    const onUp = () => {
      rz.classList.remove("dragging");
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  });
}

/* ============================ 功能区选项卡 ============================ */

function bindRibbonTabs() {
  for (const tab of document.querySelectorAll(".rtab")) {
    tab.onclick = () => {
      for (const t of document.querySelectorAll(".rtab")) {
        t.classList.toggle("active", t === tab);
        t.setAttribute("aria-selected", t === tab ? "true" : "false");
      }
      for (const p of document.querySelectorAll(".rpanel")) {
        p.classList.toggle("active", p.dataset.panel === tab.dataset.tab);
      }
    };
  }
}

function refreshRibbonState() {
  updateBoldButton();
  $("fontColorUnderline").style.background = state.fontColor;
  $("fillColorUnderline").style.background = state.fillColor;
}

/* ============================ 抽屉:历史 / 节点 / 快照 ============================ */

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
      empty.textContent = "还没有快照。快照会保存整簿内容,可随时恢复到该时点。";
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
        }).then(() => { toast("已恢复", "ok"); return pollForce(); })
          .then(() => renderAll())
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
  const form = dlg.querySelector("form");
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

async function removeSheet(name) {
  if (!confirm(`删除工作表「${name}」及其全部内容?(作为新变更上链)`)) return;
  await api(`/v1/documents/${state.docId}/ops`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify([{ stamp: { ts: 0, author: "" }, type: "remove_sheet", sheet: name }]),
  });
  await pollForce();
  if (state.sheet === name) state.sheet = state.grid.sheets.find((s) => s.name !== name)?.name || null;
  renderAll();
}

/* ============================ 文档生命周期 / 导入导出 ============================ */

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

function download(path) {
  const a = document.createElement("a");
  a.href = path;
  a.download = "";
  document.body.appendChild(a);
  a.click();
  a.remove();
}

async function importFile(file) {
  const isCsv = /\.csv$/i.test(file.name);
  const url = `/v1/documents/${state.docId}/import/${isCsv ? "csv" : "xlsx"}` +
    (isCsv ? `?sheet=${encodeURIComponent(file.name.replace(/\.csv$/i, ""))}` : "");
  await api(url, {
    method: "POST",
    headers: { "Content-Type": "application/octet-stream" },
    body: isCsv ? await file.text() : await file.arrayBuffer(),
  });
  await pollForce();
  renderAll();
  toast(`已导入 ${file.name}`, "ok");
}

/* ============================ 键盘 ============================ */

function onGridKeydown(ev) {
  if (!state.docId) return;
  if (state.editing) return; // 编辑态由编辑框自身处理
  const has = !!state.sel;
  switch (ev.key) {
    case "ArrowUp": moveSel(-1, 0, ev.shiftKey); ev.preventDefault(); return;
    case "ArrowDown": moveSel(1, 0, ev.shiftKey); ev.preventDefault(); return;
    case "ArrowLeft": moveSel(0, -1, ev.shiftKey); ev.preventDefault(); return;
    case "ArrowRight": moveSel(0, 1, ev.shiftKey); ev.preventDefault(); return;
    case "Tab": moveSel(0, ev.shiftKey ? -1 : 1); ev.preventDefault(); return;
    case "Enter":
      if (has) openEditor();
      ev.preventDefault();
      return;
    case "F2":
      if (has) openEditor();
      ev.preventDefault();
      return;
    case "Delete":
    case "Backspace":
      clearContents();
      ev.preventDefault();
      return;
    case "b": case "B":
      if (ev.ctrlKey || ev.metaKey) { applyStyleToSelection({ bold: true }); ev.preventDefault(); }
      return;
    case "c": case "C":
      if (ev.ctrlKey || ev.metaKey) { copySelection(); ev.preventDefault(); }
      return;
    case "v": case "V":
      if (ev.ctrlKey || ev.metaKey) { pasteClipboard(); ev.preventDefault(); }
      return;
    case "x": case "X":
      if (ev.ctrlKey || ev.metaKey) { copySelection(); clearContents(); ev.preventDefault(); }
      return;
    default:
      if (has && ev.key.length === 1 && !ev.ctrlKey && !ev.metaKey && !ev.altKey) {
        openEditor(ev.key);
        ev.preventDefault();
      }
  }
}

/* ============================ 事件绑定 ============================ */

function bindEvents() {
  bindRibbonTabs();
  bindColumnResize();

  // 文档
  $("newDocBtn").onclick = newDocDialog;
  $("emptyNewBtn").onclick = newDocDialog;
  $("addSheetBtn").onclick = addSheetDialog;
  $("addSheetQuick").onclick = addSheetDialog;
  $("historyBtn").onclick = showHistory;
  $("peersBtn").onclick = showPeers;
  $("snapshotBtn").onclick = showSnapshots;
  $("drawerClose").onclick = closeDrawer;
  $("drawerBackdrop").onclick = closeDrawer;

  // 导入导出
  $("exportXlsxBtn").onclick = () => download(`/v1/documents/${state.docId}/export/xlsx`);
  $("exportCsvBtn").onclick = () => download(`/v1/documents/${state.docId}/export/csv`);
  const pickFile = () => $("fileInput").click();
  $("importXlsxBtn").onclick = pickFile;
  $("importCsvBtn").onclick = pickFile;
  $("fileInput").onchange = async (ev) => {
    const file = ev.target.files[0];
    if (!file) return;
    try { await importFile(file); } catch (e) { toast("导入失败:" + e.message, "err"); }
    ev.target.value = "";
  };

  // 编辑栏
  const input = $("formulaInput");
  input.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      closeEditor(true, "down");
      $("gridWrap").focus();
      ev.preventDefault();
    } else if (ev.key === "Tab") {
      closeEditor(true, ev.shiftKey ? "left" : "right");
      $("gridWrap").focus();
      ev.preventDefault();
    } else if (ev.key === "Escape") {
      closeEditor(false);
      syncFormulaBar();
      $("gridWrap").focus();
      ev.preventDefault();
    }
  });
  input.addEventListener("input", () => {
    // 编辑栏直接改值(未开内联编辑器时):写入锚点
    if (!state.editing && state.sel) {
      openEditor(input.value);
    } else if (state.editing) {
      $("cellEditor").value = input.value;
    }
  });

  // 单元格内编辑框
  const ed = $("cellEditor");
  ed.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      closeEditor(true, "down");
      $("gridWrap").focus();
      ev.preventDefault();
    } else if (ev.key === "Tab") {
      closeEditor(true, ev.shiftKey ? "left" : "right");
      $("gridWrap").focus();
      ev.preventDefault();
    } else if (ev.key === "Escape") {
      closeEditor(false);
      syncFormulaBar();
      $("gridWrap").focus();
      ev.preventDefault();
    }
  });
  ed.addEventListener("input", () => { $("formulaInput").value = ed.value; });

  // 名称框:输入 A1 / A1:B3 回车跳转
  $("nameBox").addEventListener("keydown", (ev) => {
    if (ev.key !== "Enter") return;
    const v = $("nameBox").value.trim();
    const single = parseA1(v);
    if (single) { setSelection(single.row, single.col); scrollSelIntoView(); $("gridWrap").focus(); return; }
    const m = /^([A-Za-z]\d*):([A-Za-z]\d*)$/.exec(v.replace(/\s/g, ""));
    if (m) {
      const p1 = parseA1(m[1]), p2 = parseA1(m[2]);
      if (p1 && p2) { setSelection(p1.row, p1.col, p2.row, p2.col); scrollSelIntoView(); $("gridWrap").focus(); return; }
    }
    syncFormulaBar();
  });

  // 网格键盘
  $("gridWrap").addEventListener("keydown", onGridKeydown);

  // 样式按钮
  $("boldBtn").onclick = () => applyStyleToSelection({ bold: true });
  $("alignLeftBtn").onclick = () => applyStyleToSelection({ align: "left" });
  $("alignCenterBtn").onclick = () => applyStyleToSelection({ align: "center" });
  $("alignRightBtn").onclick = () => applyStyleToSelection({ align: "right" });
  $("fontColorBtn").onclick = () => showColorPop($("fontColorBtn"), FONT_COLORS, (c) => {
    state.fontColor = c || "#e03131";
    applyStyleToSelection({ color: c });
  });
  $("fillColorBtn").onclick = () => showColorPop($("fillColorBtn"), FILL_COLORS, (c) => {
    state.fillColor = c || "#fff3bf";
    applyStyleToSelection({ bg: c });
  }, true);
  $("clearContentsBtn").onclick = clearContents;
  $("copyBtn").onclick = copySelection;
  $("pasteBtn").onclick = pasteClipboard;
  $("autoSumBtn").onclick = () => insertFunction("SUM");

  // 公式函数按钮
  for (const b of document.querySelectorAll("[data-fn]")) {
    b.onclick = () => insertFunction(b.dataset.fn);
  }
  $("funcHelpBtn").onclick = () => openDrawer("支持的函数", (body) => {
    body.innerHTML = `<div class="list-item"><div class="li-title">SUM / AVERAGE / MIN / MAX / COUNT / COUNTA</div>
      <div class="li-sub">区域聚合,如 =SUM(B2:B9)</div></div>
      <div class="list-item"><div class="li-title">IF / AND / OR / NOT</div>
      <div class="li-sub">逻辑判断,如 =IF(A1&gt;60,"合格","不合格")</div></div>
      <div class="list-item"><div class="li-title">ABS / ROUND / CONCAT</div>
      <div class="li-sub">数值与文本,如 =ROUND(A2,2)</div></div>
      <div class="list-item"><div class="li-title">运算符</div>
      <div class="li-sub">+ - * / ^ % &amp;(连接)= &lt;&gt; &lt; &gt; &lt;= &gt;=</div></div>
      <div class="list-item"><div class="li-title">单元格引用</div>
      <div class="li-sub">A1、$A$1、Sheet2!B3、A1:C9(跨表引用表名区分大小写)</div></div>`;
  });

  // 视图
  $("showFormulaChk").onchange = (ev) => {
    state.showFormula = ev.target.checked;
    renderGrid();
    updateSelectionUI();
  };
  const applyZoom = (z) => {
    state.zoom = Math.min(1.5, Math.max(0.8, z));
    $("gridHost").style.zoom = state.zoom;
    $("zoomVal").textContent = Math.round(state.zoom * 100) + "%";
    $("zoomSel").value = String(state.zoom);
  };
  $("zoomSel").onchange = (ev) => applyZoom(parseFloat(ev.target.value));
  $("zoomIn").onclick = () => applyZoom(state.zoom + 0.1);
  $("zoomOut").onclick = () => applyZoom(state.zoom - 0.1);

  // 全局:关闭浮层 / 菜单
  document.addEventListener("mousedown", (ev) => {
    if (!ev.target.closest(".ctx-menu")) hideCtxMenu();
    if (!ev.target.closest(".color-pop") && !ev.target.closest(".dd-btn")) hideColorPop();
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") { hideCtxMenu(); hideColorPop(); }
  });
}

/* ============================ 启动 ============================ */

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
  renderAll();
  markSync();
  setInterval(poll, 2500);
  setInterval(pollPeers, 5000);
  pollPeers();
}

boot();
