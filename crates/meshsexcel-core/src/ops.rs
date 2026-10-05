use serde::{Deserialize, Serialize};

/// 单元格样式(MVP:bold / 字体色 / 背景色 / 水平对齐)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CellStyle {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
}

impl CellStyle {
    /// 是否为"无样式"。
    pub fn is_empty(&self) -> bool {
        !self.bold && self.color.is_none() && self.bg.is_none() && self.align.is_none()
    }
}

/// LWW 时间戳:`(ts, author)` 字典序,大者胜。
///
/// 同一元素的两条并发操作只需比较该二元组即可在所有节点上得到一致的
/// 合并结果(与到达顺序无关),这是 MeshSExcel 协同收敛的核心约定。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OpStamp {
    pub ts: i64,
    pub author: String,
}

impl OpStamp {
    pub fn new(ts: i64, author: impl Into<String>) -> Self {
        Self {
            ts,
            author: author.into(),
        }
    }
}

/// 具体操作体。信封见 [`CellOp`]。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpKind {
    /// 创建文档(首个 block 的第一个 op;远端节点据此物化文档)。
    CreateDocument { name: String, owner: String },
    /// 设置单元格:值为字面量,公式以 "=" 开头,二者可并存
    /// (formula 参与计算,value 作为字面量存档)。
    SetCell {
        sheet: String,
        row: u32,
        col: u32,
        #[serde(default)]
        value: Option<String>,
        #[serde(default)]
        formula: Option<String>,
        #[serde(default)]
        style: Option<CellStyle>,
    },
    /// 新增工作表;position 可省略(追加到末尾)。
    AddSheet {
        sheet: String,
        #[serde(default)]
        position: Option<usize>,
    },
    /// 删除工作表(同时清除其全部单元格)。
    RemoveSheet { sheet: String },
}

/// LWW 操作信封:所有节点以 `(stamp, kind)` 的顺序确定性地应用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellOp {
    pub stamp: OpStamp,
    #[serde(flatten)]
    pub kind: OpKind,
}

impl CellOp {
    pub fn new(ts: i64, author: impl Into<String>, kind: OpKind) -> Self {
        Self {
            stamp: OpStamp::new(ts, author),
            kind,
        }
    }

    /// 生成审计用的人类可读摘要(写入 block.payload.operations)。
    pub fn describe(&self) -> String {
        match &self.kind {
            OpKind::CreateDocument { name, owner } => {
                format!("create_document name={} owner={}", name, owner)
            }
            OpKind::SetCell {
                sheet,
                row,
                col,
                value,
                formula,
                ..
            } => {
                if let Some(f) = formula {
                    format!("set {sheet}!{}{row} = {f}", col_name(*col))
                } else {
                    format!(
                        "set {sheet}!{}{row} = {}",
                        col_name(*col),
                        value.as_deref().unwrap_or("")
                    )
                }
            }
            OpKind::AddSheet { sheet, .. } => format!("add_sheet {sheet}"),
            OpKind::RemoveSheet { sheet } => format!("remove_sheet {sheet}"),
        }
    }
}

/// 列号(1-based)转 Excel 列名:1→A, 26→Z, 27→AA。
pub fn col_name(col: u32) -> String {
    let mut n = col;
    let mut out = Vec::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        out.push(b'A' + rem);
        n = (n - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Excel 列名转列号(1-based):A→1, Z→26, AA→27;非法输入返回 None。
pub fn parse_col_name(name: &str) -> Option<u32> {
    if name.is_empty() || name.len() > 3 {
        return None;
    }
    let mut n: u32 = 0;
    for ch in name.chars() {
        let up = ch.to_ascii_uppercase();
        if !up.is_ascii_uppercase() {
            return None;
        }
        n = n * 26 + (up as u32 - 'A' as u32 + 1);
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn col_name_roundtrip() {
        assert_eq!(col_name(1), "A");
        assert_eq!(col_name(26), "Z");
        assert_eq!(col_name(27), "AA");
        assert_eq!(col_name(52), "AZ");
        assert_eq!(col_name(53), "BA");
        for c in [1u32, 2, 25, 26, 27, 300, 700] {
            assert_eq!(parse_col_name(&col_name(c)), Some(c));
        }
    }

    #[test]
    fn stamp_ordering() {
        let a = OpStamp::new(100, "alice");
        let b = OpStamp::new(100, "bob");
        let c = OpStamp::new(101, "alice");
        assert!(b > a, "同 ts 按 author 字典序定胜负,保证全序");
        assert!(c > b, "新 ts 恒胜");
    }

    #[test]
    fn op_describe_and_serde() {
        let op = CellOp::new(
            5,
            "alice",
            OpKind::SetCell {
                sheet: "Sheet1".into(),
                row: 2,
                col: 3,
                value: Some("42".into()),
                formula: None,
                style: None,
            },
        );
        assert_eq!(op.describe(), "set Sheet1!C2 = 42");
        let json = serde_json::to_string(&op).unwrap();
        let back: CellOp = serde_json::from_str(&json).unwrap();
        assert_eq!(op, back);
    }
}
