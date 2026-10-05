//! 工作簿内存状态:应用 LWW 操作、维护公式依赖图并增量重算。
//!
//! 合并收敛的关键:任何节点无论以什么顺序收到同一批 [`CellOp`],
//! 只要按 `(stamp, kind)` 应用,LWW 规则保证最终状态一致。

use crate::formula::{self, Ast, FormulaContext, Value};
use crate::ops::{CellOp, CellStyle, OpKind, OpStamp};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};

/// 单元格键:(表名, 行, 列),行列均 1-based。
pub type CellKey = (String, u32, u32);

/// 单元格的 LWW 记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellRecord {
    pub stamp: OpStamp,
    pub value: Option<String>,
    pub formula: Option<String>,
    pub style: Option<CellStyle>,
}

/// 工作簿:文档的内存态(真源是 block 链,可由其完整重建;不直接序列化)。
#[derive(Debug, Clone)]
pub struct Workbook {
    pub doc_id: String,
    pub name: String,
    pub owner: String,
    /// 现存工作表(有序)。
    sheets: Vec<String>,
    sheet_stamps: HashMap<String, OpStamp>,
    cells: HashMap<CellKey, CellRecord>,
    /// 公式 AST 缓存(仅公式格有条目);None = 公式解析失败。
    ast_cache: HashMap<CellKey, Option<Ast>>,
    computed: HashMap<CellKey, Value>,
    /// 反向依赖:被引用格 → 引用它的公式格集合。
    dependents: HashMap<CellKey, BTreeSet<CellKey>>,
    /// 正向依赖:公式格 → 它引用的格(用于撤边)。
    refs_of: HashMap<CellKey, Vec<CellKey>>,
    created: bool,
}

const MAX_EVAL_DEPTH: u32 = 256;

/// 全空记录(清除操作的墓碑):必须保留以维持 LWW 时序,
/// 但对网格 / 导出 / 缓存不可见。
pub fn is_tombstone(rec: &CellRecord) -> bool {
    rec.value.is_none() && rec.formula.is_none() && rec.style.is_none()
}

impl Workbook {
    pub fn new(doc_id: impl Into<String>) -> Self {
        Self {
            doc_id: doc_id.into(),
            name: String::new(),
            owner: String::new(),
            sheets: Vec::new(),
            sheet_stamps: HashMap::new(),
            cells: HashMap::new(),
            ast_cache: HashMap::new(),
            computed: HashMap::new(),
            dependents: HashMap::new(),
            refs_of: HashMap::new(),
            created: false,
        }
    }

    /// 应用一批操作(按给定顺序),返回应用的条数并触发增量重算。
    pub fn apply_ops(&mut self, ops: &[CellOp]) -> usize {
        let mut dirty: Vec<CellKey> = Vec::new();
        let mut structural = false;
        for op in ops {
            match self.apply_op(op) {
                Applied::Cell(key) => dirty.push(key),
                Applied::Structure => structural = true,
                Applied::Ignored => {}
            }
        }
        if structural || !dirty.is_empty() {
            // 结构变化(表增删)会影响所有公式的 #REF! 判定,全量重算;
            // MVP 数据量下可接受,且只在表增删时发生。
            if structural {
                let all: Vec<CellKey> = self.cells.keys().cloned().collect();
                self.recompute(&all);
            } else {
                self.recompute(&dirty);
            }
        }
        dirty.len()
    }

    fn apply_op(&mut self, op: &CellOp) -> Applied {
        match &op.kind {
            OpKind::CreateDocument { name, owner } => {
                if self.created {
                    return Applied::Ignored;
                }
                self.created = true;
                self.name = name.clone();
                self.owner = owner.clone();
                Applied::Structure
            }
            OpKind::AddSheet { sheet, position } => {
                if self.sheet_stamps.get(sheet).is_some_and(|s| *s >= op.stamp) {
                    return Applied::Ignored;
                }
                self.sheet_stamps.insert(sheet.clone(), op.stamp.clone());
                if !self.sheets.contains(sheet) {
                    let pos = position.unwrap_or(self.sheets.len()).min(self.sheets.len());
                    self.sheets.insert(pos, sheet.clone());
                }
                Applied::Structure
            }
            OpKind::RemoveSheet { sheet } => {
                if self.sheet_stamps.get(sheet).is_some_and(|s| *s >= op.stamp) {
                    return Applied::Ignored;
                }
                self.sheet_stamps.insert(sheet.clone(), op.stamp.clone());
                if let Some(pos) = self.sheets.iter().position(|s| s == sheet) {
                    self.sheets.remove(pos);
                }
                let removed: Vec<CellKey> = self
                    .cells
                    .keys()
                    .filter(|k| k.0 == *sheet)
                    .cloned()
                    .collect();
                for key in &removed {
                    self.drop_cell_layers(key);
                }
                Applied::Structure
            }
            OpKind::SetCell {
                sheet,
                row,
                col,
                value,
                formula,
                style,
            } => {
                // 注意:gossip 不保证 block 顺序,SetCell 可能先于 AddSheet 到达;
                // 一律按 LWW 应用(缺表时仅不可见),否则乱序场景会永久丢格
                let key = (sheet.clone(), *row, *col);
                if let Some(existing) = self.cells.get(&key) {
                    if existing.stamp >= op.stamp {
                        return Applied::Ignored;
                    }
                }
                let record = CellRecord {
                    stamp: op.stamp.clone(),
                    value: value.clone(),
                    formula: formula.clone(),
                    style: style.clone(),
                };
                self.cells.insert(key.clone(), record);
                Applied::Cell(key)
            }
        }
    }

    fn drop_cell_layers(&mut self, key: &CellKey) {
        self.remove_edges(key);
        self.cells.remove(key);
        self.ast_cache.remove(key);
        self.computed.remove(key);
        self.dependents.remove(key);
    }

    fn remove_edges(&mut self, key: &CellKey) {
        if let Some(refs) = self.refs_of.remove(key) {
            for r in refs {
                if let Some(set) = self.dependents.get_mut(&r) {
                    set.remove(key);
                    if set.is_empty() {
                        self.dependents.remove(&r);
                    }
                }
            }
        }
    }

    /// 重建 key 的公式边(公式变化/新增/清除时调用)。
    fn reedge(&mut self, key: &CellKey) {
        self.remove_edges(key);
        let ast = match self.cells.get(key).and_then(|r| r.formula.as_ref()) {
            Some(f) => formula::parse(f).ok(),
            // 公式被清除:必须连 AST 缓存一起删,否则旧公式会阴魂不散
            None => {
                self.ast_cache.remove(key);
                return;
            }
        };
        if let Some(ast) = &ast {
            let refs: Vec<CellKey> = formula::extract_refs(ast)
                .into_iter()
                .map(|(s, r, c)| (s.unwrap_or_else(|| key.0.clone()), r, c))
                .collect();
            for r in &refs {
                self.dependents
                    .entry(r.clone())
                    .or_default()
                    .insert(key.clone());
            }
            self.refs_of.insert(key.clone(), refs);
        }
        self.ast_cache.insert(key.clone(), ast);
    }

    /// 以 dirty 集为种子,重算其传递闭包(增量公式重算核心)。
    fn recompute(&mut self, dirty: &[CellKey]) {
        for key in dirty {
            if self.cells.contains_key(key) {
                self.reedge(key);
            }
        }
        // 收集传递依赖闭包
        let mut closure: Vec<CellKey> = Vec::new();
        let mut seen: HashSet<CellKey> = HashSet::new();
        let mut queue: Vec<CellKey> = dirty.to_vec();
        for k in &queue {
            seen.insert(k.clone());
        }
        while let Some(k) = queue.pop() {
            closure.push(k.clone());
            if let Some(deps) = self.dependents.get(&k) {
                for d in deps {
                    if seen.insert(d.clone()) {
                        queue.push(d.clone());
                    }
                }
            }
        }
        // 清空闭包内的旧计算值,再逐格求值
        for k in &closure {
            self.computed.remove(k);
        }
        let mut run = EvalRun {
            memo: HashMap::new(),
            visiting: HashSet::new(),
        };
        for key in &closure {
            eval_key(self, key, &mut run, 0);
        }
        self.computed.extend(run.memo);
    }

    /// 某格的"原始字面量值"(不经过公式)。
    /// 输入层语义与 Excel 一致:数字串即 Number,TRUE/FALSE 即 Bool,其余为 Text。
    fn raw_value(&self, key: &CellKey) -> Value {
        match self.cells.get(key).and_then(|r| r.value.clone()) {
            None => Value::Empty,
            Some(s) => {
                let t = s.trim();
                if t.eq_ignore_ascii_case("TRUE") {
                    Value::Bool(true)
                } else if t.eq_ignore_ascii_case("FALSE") {
                    Value::Bool(false)
                } else if let Ok(n) = t.parse::<f64>() {
                    Value::Number(n)
                } else {
                    Value::Text(s)
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // 只读访问
    // ------------------------------------------------------------------

    pub fn sheet_names(&self) -> Vec<String> {
        self.sheets.clone()
    }

    pub fn sheet_exists(&self, sheet: &str) -> bool {
        self.sheets.iter().any(|s| s == sheet)
    }

    /// 某表内全部单元格(按行、列排序)。
    pub fn cells_in_sheet(&self, sheet: &str) -> Vec<(u32, u32, &CellRecord)> {
        let mut rows: Vec<(u32, u32, &CellRecord)> = self
            .cells
            .iter()
            // 表不存在时整表不可见(SetCell 可能先于 AddSheet 到达,格已落但先藏着)
            .filter(|(k, v)| {
                k.0 == sheet && self.sheets.iter().any(|s| s == sheet) && !is_tombstone(v)
            })
            .map(|(k, v)| (k.1, k.2, v))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        rows
    }

    pub fn cell_record(&self, sheet: &str, row: u32, col: u32) -> Option<&CellRecord> {
        self.cells.get(&(sheet.to_string(), row, col))
    }

    /// 计算后的值(UI / 导出用)。
    pub fn computed_value(&self, sheet: &str, row: u32, col: u32) -> Value {
        let key = (sheet.to_string(), row, col);
        if let Some(v) = self.computed.get(&key) {
            return v.clone();
        }
        if self.ast_cache.contains_key(&key) {
            // 是公式格但没算过:按字面量兜底不应发生,给 NAME 以示异常
            return Value::err_name();
        }
        self.raw_value(&key)
    }

    /// UI 显示文本。
    pub fn cell_display(&self, sheet: &str, row: u32, col: u32) -> String {
        self.computed_value(sheet, row, col).display()
    }

    /// 全部单元格(供 cells_cache 落库 / 快照)。
    pub fn all_cells(&self) -> impl Iterator<Item = (&CellKey, &CellRecord)> {
        self.cells.iter()
    }

    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }
}

#[derive(Debug)]
enum Applied {
    Cell(CellKey),
    Structure,
    Ignored,
}

// ---------------------------------------------------------------------------
// 求值桥:Workbook 作为 FormulaContext(带 visiting 环检测 + memo)
// ---------------------------------------------------------------------------

/// 一轮重算的运行态。
struct EvalRun {
    memo: HashMap<CellKey, Value>,
    visiting: HashSet<CellKey>,
}

struct EvalCtx<'a> {
    wb: &'a Workbook,
    run: RefCell<&'a mut EvalRun>,
    depth: u32,
}

impl FormulaContext for EvalCtx<'_> {
    fn cell_value(&self, sheet: &str, row: u32, col: u32) -> Value {
        let key = (sheet.to_string(), row, col);
        let mut run = self.run.borrow_mut();
        eval_key(self.wb, &key, &mut run, self.depth + 1)
    }
    fn sheet_exists(&self, sheet: &str) -> bool {
        self.wb.sheet_exists(sheet)
    }
}

fn eval_key(wb: &Workbook, key: &CellKey, run: &mut EvalRun, depth: u32) -> Value {
    if let Some(v) = run.memo.get(key) {
        return v.clone();
    }
    if let Some(v) = wb.computed.get(key) {
        return v.clone();
    }
    // ast_cache 没有该键 ⇒ 非公式格,取字面量
    let Some(cell_ast) = wb.ast_cache.get(key) else {
        let v = wb.raw_value(key);
        run.memo.insert(key.clone(), v.clone());
        return v;
    };
    // 公式存在但解析失败
    let Some(ast) = cell_ast else {
        let v = Value::err_name();
        run.memo.insert(key.clone(), v.clone());
        return v;
    };
    if run.visiting.contains(key) {
        return Value::err_circ();
    }
    if depth > MAX_EVAL_DEPTH {
        let v = Value::err_circ();
        run.memo.insert(key.clone(), v.clone());
        return v;
    }
    run.visiting.insert(key.clone());
    let v = {
        let ctx = EvalCtx {
            wb,
            run: RefCell::new(&mut *run),
            depth,
        };
        formula::evaluate(ast, &key.0, &ctx)
    };
    run.visiting.remove(key);
    run.memo.insert(key.clone(), v.clone());
    v
}

// ---------------------------------------------------------------------------
// 快照
// ---------------------------------------------------------------------------

/// 单个单元格的快照行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotCell {
    pub row: u32,
    pub col: u32,
    pub value: Option<String>,
    pub formula: Option<String>,
    pub style: Option<CellStyle>,
}

/// 一张表的快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SheetSnapshot {
    pub name: String,
    pub cells: Vec<SnapshotCell>,
}

// ---------------------------------------------------------------------------
// 网格输出(API 用)
// ---------------------------------------------------------------------------

/// 一个格子的计算视图(value 是计算后的显示文本)。
#[derive(Debug, Clone, Serialize)]
pub struct GridCell {
    pub row: u32,
    pub col: u32,
    pub value: String,
    pub formula: Option<String>,
    pub style: Option<CellStyle>,
}

/// 一张表的网格(rows/cols 为单元格覆盖范围,空表为 0)。
#[derive(Debug, Clone, Serialize)]
pub struct GridSheet {
    pub name: String,
    pub rows: u32,
    pub cols: u32,
    pub cells: Vec<GridCell>,
}

/// 整簿网格。
#[derive(Debug, Clone, Serialize)]
pub struct WorkbookGrid {
    pub doc_id: String,
    pub name: String,
    pub sheets: Vec<GridSheet>,
}

impl Workbook {
    /// 导出全簿计算网格(REST /cells 响应体)。
    pub fn grid(&self) -> WorkbookGrid {
        let sheets = self
            .sheet_names()
            .into_iter()
            .map(|name| {
                let cells = self.cells_in_sheet(&name);
                let rows = cells.last().map(|c| c.0).unwrap_or(0);
                let cols = cells.iter().map(|c| c.1).max().unwrap_or(0);
                let out = cells
                    .into_iter()
                    .map(|(row, col, rec)| GridCell {
                        row,
                        col,
                        value: self.cell_display(&name, row, col),
                        formula: rec.formula.clone(),
                        style: rec.style.clone(),
                    })
                    .collect();
                GridSheet {
                    name,
                    rows,
                    cols,
                    cells: out,
                }
            })
            .collect();
        WorkbookGrid {
            doc_id: self.doc_id.clone(),
            name: self.name.clone(),
            sheets,
        }
    }

    /// 导出全簿快照(备份用;恢复 = 把这些格子以新 stamp 重新提交)。
    pub fn snapshot(&self) -> Vec<SheetSnapshot> {
        self.sheet_names()
            .into_iter()
            .map(|name| SheetSnapshot {
                cells: self
                    .cells_in_sheet(&name)
                    .into_iter()
                    .map(|(row, col, rec)| SnapshotCell {
                        row,
                        col,
                        value: rec.value.clone(),
                        formula: rec.formula.clone(),
                        style: rec.style.clone(),
                    })
                    .collect(),
                name,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{col_name, parse_col_name};

    fn author(n: &str) -> String {
        n.to_string()
    }

    fn set(ts: i64, who: &str, sheet: &str, a1: &str, value: &str) -> CellOp {
        let (row, col) = parse_a1(a1).unwrap();
        CellOp::new(
            ts,
            author(who),
            OpKind::SetCell {
                sheet: sheet.into(),
                row,
                col,
                value: Some(value.into()),
                formula: None,
                style: None,
            },
        )
    }

    fn set_formula(ts: i64, who: &str, sheet: &str, a1: &str, f: &str) -> CellOp {
        let (row, col) = parse_a1(a1).unwrap();
        CellOp::new(
            ts,
            author(who),
            OpKind::SetCell {
                sheet: sheet.into(),
                row,
                col,
                value: None,
                formula: Some(f.into()),
                style: None,
            },
        )
    }

    fn parse_a1(a1: &str) -> Option<(u32, u32)> {
        let split = a1.find(|c: char| c.is_ascii_digit())?;
        let (letters, digits) = a1.split_at(split);
        Some((digits.parse().ok()?, parse_col_name(letters)?))
    }

    fn new_wb() -> Workbook {
        let mut wb = Workbook::new("doc-test");
        wb.apply_ops(&[CellOp::new(
            0,
            "sys",
            OpKind::CreateDocument {
                name: "测试表".into(),
                owner: "alice".into(),
            },
        )]);
        wb.apply_ops(&[CellOp::new(
            1,
            "sys",
            OpKind::AddSheet {
                sheet: crate::DEFAULT_SHEET.into(),
                position: None,
            },
        )]);
        wb
    }

    #[test]
    fn lww_converges_regardless_of_order() {
        let ops = vec![
            set(100, "alice", "Sheet1", "A1", "from-alice"),
            set(200, "bob", "Sheet1", "A1", "from-bob"),
        ];
        let mut wb1 = new_wb();
        wb1.apply_ops(&ops);
        let mut wb2 = new_wb();
        wb2.apply_ops(ops.iter().rev().cloned().collect::<Vec<_>>().as_slice());
        let expected = "from-bob";
        assert_eq!(wb1.cell_display("Sheet1", 1, 1), expected);
        assert_eq!(wb2.cell_display("Sheet1", 1, 1), expected, "乱序到达也收敛");
    }

    #[test]
    fn formula_dependency_recompute() {
        let mut wb = new_wb();
        wb.apply_ops(&[
            set(1, "a", "Sheet1", "A1", "10"),
            set(2, "a", "Sheet1", "A2", "20"),
        ]);
        wb.apply_ops(&[set_formula(3, "a", "Sheet1", "B1", "=SUM(A1:A2)")]);
        assert_eq!(wb.cell_display("Sheet1", 1, 2), "30");
        // 改 A1 → B1 自动跟着变
        wb.apply_ops(&[set(4, "a", "Sheet1", "A1", "15")]);
        assert_eq!(
            wb.cell_display("Sheet1", 1, 2),
            "35",
            "增量重算:改 A1 后 B1 应为 35"
        );
        // 删除公式
        wb.apply_ops(&[set(5, "a", "Sheet1", "B1", "")]);
        assert_eq!(wb.cell_display("Sheet1", 1, 2), "");
    }

    #[test]
    fn circular_reference_detected() {
        let mut wb = new_wb();
        wb.apply_ops(&[
            set_formula(1, "a", "Sheet1", "A1", "=B1+1"),
            set_formula(2, "a", "Sheet1", "B1", "=A1+1"),
        ]);
        assert_eq!(wb.cell_display("Sheet1", 1, 1), "#CIRC!");
        assert_eq!(wb.cell_display("Sheet1", 1, 2), "#CIRC!");
    }

    #[test]
    fn cross_sheet_ref_and_ref_edit() {
        let mut wb = new_wb();
        wb.apply_ops(&[CellOp::new(
            3,
            "a",
            OpKind::AddSheet {
                sheet: "Data".into(),
                position: None,
            },
        )]);
        wb.apply_ops(&[set(4, "a", "Data", "A1", "7")]);
        wb.apply_ops(&[set_formula(5, "a", "Sheet1", "A1", "=Data!A1*2")]);
        assert_eq!(wb.cell_display("Sheet1", 1, 1), "14");
        // 换公式指向别处:旧依赖边要撤掉
        wb.apply_ops(&[set_formula(6, "a", "Sheet1", "A1", "=Data!A1*3")]);
        wb.apply_ops(&[set(7, "a", "Data", "A1", "1")]);
        assert_eq!(wb.cell_display("Sheet1", 1, 1), "3");
    }

    #[test]
    fn remove_sheet_clears_cells() {
        let mut wb = new_wb();
        wb.apply_ops(&[CellOp::new(
            3,
            "a",
            OpKind::AddSheet {
                sheet: "Tmp".into(),
                position: None,
            },
        )]);
        wb.apply_ops(&[set(4, "a", "Tmp", "A1", "x")]);
        assert!(wb.sheet_exists("Tmp"));
        wb.apply_ops(&[CellOp::new(
            5,
            "a",
            OpKind::RemoveSheet {
                sheet: "Tmp".into(),
            },
        )]);
        assert!(!wb.sheet_exists("Tmp"));
        assert_eq!(wb.cell_count(), 0, "删表连带清格");
    }

    #[test]
    fn set_cell_on_missing_sheet_applies_but_hidden() {
        // gossip 乱序:SetCell 先于 AddSheet 到达也必须落格(否则永久丢失)
        let mut wb = new_wb();
        let n = wb.apply_ops(&[set(1, "a", "NoSuchSheet", "A1", "x")]);
        assert_eq!(n, 1);
        assert!(!wb.sheet_exists("NoSuchSheet"));
        assert_eq!(
            wb.cells_in_sheet("NoSuchSheet").len(),
            0,
            "缺表时对外不可见"
        );
        // 表随后创建,格子立即可见
        wb.apply_ops(&[CellOp::new(
            2,
            "a",
            OpKind::AddSheet {
                sheet: "NoSuchSheet".into(),
                position: None,
            },
        )]);
        assert_eq!(wb.cell_display("NoSuchSheet", 1, 1), "x");
    }

    #[test]
    fn rebuild_from_ops_is_identical() {
        let ops = vec![
            set(1, "a", "Sheet1", "A1", "5"),
            set(2, "a", "Sheet1", "A2", "6"),
            set_formula(3, "b", "Sheet1", "B1", "=A1+A2"),
            set(4, "b", "Sheet1", "A1", "1"),
        ];
        let mut wb = new_wb();
        wb.apply_ops(&ops);
        // 模拟重启:全部重放
        let mut wb2 = new_wb();
        wb2.apply_ops(&ops);
        assert_eq!(
            wb.cell_display("Sheet1", 1, 2),
            wb2.cell_display("Sheet1", 1, 2),
            "重放 block 全量 ops 应得到一致状态"
        );
        assert_eq!(wb.cell_display("Sheet1", 1, 2), "7");
    }

    #[test]
    fn snapshot_covers_all_sheets() {
        let mut wb = new_wb();
        wb.apply_ops(&[set(1, "a", "Sheet1", "A1", "v")]);
        let snap = wb.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].cells.len(), 1);
        assert_eq!(snap[0].cells[0].value.as_deref(), Some("v"));
    }

    #[test]
    fn col_name_import_works() {
        assert_eq!(col_name(3), "C");
        assert_eq!(parse_col_name("C"), Some(3));
    }
}
