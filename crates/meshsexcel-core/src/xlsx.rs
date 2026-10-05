//! XLSX / CSV 导入导出。
//!
//! 导出:公式写成真公式(带缓存计算结果),Excel / WPS 打开可直接重算;
//! 导入:读出每个非空单元格,交给上层转成 [`crate::ops::CellOp`] 走 block 链。

use crate::formula::format_number;
use crate::ops::CellStyle;
use crate::sheet::Workbook;
use crate::{Error, Result};
use calamine::{Data, Reader, Xlsx};
use rust_xlsxwriter::{Format, FormatAlign, Workbook as XlsxBook};
use std::io::Cursor;

/// 导入结果:一张表 + 它的全部非空单元格。
#[derive(Debug, Clone)]
pub struct ImportedSheet {
    pub name: String,
    pub cells: Vec<ImportedCell>,
}

/// 导入的单元格(行列 1-based)。
#[derive(Debug, Clone)]
pub struct ImportedCell {
    pub row: u32,
    pub col: u32,
    pub value: Option<String>,
    pub formula: Option<String>,
}

/// 工作簿 → XLSX 字节。
pub fn workbook_to_xlsx(wb: &Workbook) -> Result<Vec<u8>> {
    let mut book = XlsxBook::new();
    for name in wb.sheet_names() {
        let ws = book.add_worksheet();
        ws.set_name(&name)
            .map_err(|e| Error::Format(format!("set sheet name: {e}")))?;
        for (row, col, rec) in wb.cells_in_sheet(&name) {
            let r = row - 1; // RowNum = u32
            let c = (col - 1) as u16; // ColNum = u16
            let format = format_for(rec.style.as_ref());
            match (&rec.formula, &rec.value) {
                (Some(f), _) => {
                    let formula = if f.starts_with('=') {
                        f.clone()
                    } else {
                        format!("={f}")
                    };
                    ws.write_formula_with_format(r, c, formula.as_str(), &format)
                        .map_err(xlsx_err)?;
                    let shown = wb.computed_value(&name, row, col).display();
                    let _ = ws.set_formula_result(r, c, shown);
                }
                (None, Some(v)) => {
                    write_literal(ws, r, c, v, &format)?;
                }
                (None, None) => {
                    // 只有样式的格子:空串保住样式
                    ws.write_string_with_format(r, c, "", &format)
                        .map_err(xlsx_err)?;
                }
            }
        }
    }
    book.save_to_buffer().map_err(xlsx_err)
}

/// XLSX 字节 → 每张表的非空单元格。
pub fn xlsx_to_sheets(data: &[u8]) -> Result<Vec<ImportedSheet>> {
    let mut reader: Xlsx<Cursor<&[u8]>> =
        Xlsx::new(Cursor::new(data)).map_err(|e| Error::Format(format!("open xlsx: {e}")))?;
    let names = reader.sheet_names().to_vec();
    let mut out = Vec::new();
    for name in names {
        let range = reader
            .worksheet_range(&name)
            .map_err(|e| Error::Format(format!("read sheet {name}: {e}")))?;
        let formulas = reader
            .worksheet_formula(&name)
            .unwrap_or_else(|_| calamine::Range::empty());
        let mut cells = Vec::new();
        let (height, width) = range.get_size();
        for (ri, row) in range.rows().enumerate() {
            for (ci, data) in row.iter().enumerate() {
                if matches!(data, Data::Empty) {
                    continue;
                }
                let row = ri as u32 + 1;
                let col = ci as u32 + 1;
                let formula = formulas.get_value((ri as u32, ci as u32)).map(|s| {
                    if s.starts_with('=') {
                        s.to_string()
                    } else {
                        format!("={s}")
                    }
                });
                let value = data_to_string(data);
                if value.is_none() && formula.is_none() {
                    continue;
                }
                cells.push(ImportedCell {
                    row,
                    col,
                    value,
                    formula,
                });
            }
        }
        let _ = (height, width);
        out.push(ImportedSheet { name, cells });
    }
    Ok(out)
}

/// CSV 文本 → 网格(值层面;CSV 没有公式与多表)。
pub fn csv_to_grid(text: &str) -> Result<Vec<Vec<String>>> {
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(false)
        .from_reader(text.as_bytes());
    let mut grid = Vec::new();
    for record in rdr.records() {
        let record = record.map_err(|e| Error::Format(format!("csv: {e}")))?;
        grid.push(record.into_iter().map(|s| s.to_string()).collect());
    }
    Ok(grid)
}

/// 网格 → CSV 文本。
pub fn grid_to_csv(grid: &[Vec<String>]) -> String {
    let mut buf = Vec::new();
    {
        let mut wtr = csv::Writer::from_writer(&mut buf);
        for row in grid {
            let _ = wtr.write_record(row);
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// 工作簿某表 → CSV 文本(取计算值)。
pub fn workbook_sheet_to_csv(wb: &Workbook, sheet: &str) -> Result<String> {
    if !wb.sheet_exists(sheet) {
        return Err(Error::NotFound(format!("sheet {sheet}")));
    }
    let cells = wb.cells_in_sheet(sheet);
    let max_row = cells.last().map(|c| c.0).unwrap_or(0);
    let max_col = cells.iter().map(|c| c.1).max().unwrap_or(0);
    let mut grid = vec![vec![String::new(); max_col as usize]; max_row as usize];
    for (row, col, _) in &cells {
        grid[(*row - 1) as usize][(*col - 1) as usize] = wb.cell_display(sheet, *row, *col);
    }
    Ok(grid_to_csv(&grid))
}

fn write_literal(
    ws: &mut rust_xlsxwriter::Worksheet,
    r: u32,
    c: u16,
    v: &str,
    format: &Format,
) -> Result<()> {
    if let Ok(n) = v.trim().parse::<f64>() {
        ws.write_number_with_format(r, c, n, format)
            .map_err(xlsx_err)?;
    } else if v == "TRUE" {
        ws.write_boolean_with_format(r, c, true, format)
            .map_err(xlsx_err)?;
    } else if v == "FALSE" {
        ws.write_boolean_with_format(r, c, false, format)
            .map_err(xlsx_err)?;
    } else {
        ws.write_string_with_format(r, c, v, format)
            .map_err(xlsx_err)?;
    }
    Ok(())
}

fn format_for(style: Option<&CellStyle>) -> Format {
    let Some(style) = style else {
        return Format::new();
    };
    // rust_xlsxwriter 的 Format setter 是 builder 风格(消费 self)
    let mut f = Format::new();
    if style.bold {
        f = f.set_bold();
    }
    if let Some(color) = &style.color {
        f = f.set_font_color(color.as_str());
    }
    if let Some(bg) = &style.bg {
        f = f.set_background_color(bg.as_str());
    }
    if let Some(align) = &style.align {
        f = match align.as_str() {
            "center" => f.set_align(FormatAlign::Center),
            "right" => f.set_align(FormatAlign::Right),
            "left" => f.set_align(FormatAlign::Left),
            _ => f,
        };
    }
    f
}

fn data_to_string(data: &Data) -> Option<String> {
    match data {
        Data::Empty => None,
        Data::Float(f) => Some(format_number(*f)),
        Data::Int(i) => Some(format_number(*i as f64)),
        Data::String(s) => Some(s.clone()),
        Data::Bool(b) => Some((if *b { "TRUE" } else { "FALSE" }).into()),
        Data::DateTime(dt) => Some(format_number(dt.as_f64())),
        Data::Error(e) => Some(format!("#{e}")),
        other => {
            // 未来新增 Data 变体:退化为 debug 文本
            Some(format!("{other:?}"))
        }
    }
}

fn xlsx_err(e: rust_xlsxwriter::XlsxError) -> Error {
    Error::Format(format!("xlsx: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{CellOp, OpKind};
    use crate::{new_document_id, DEFAULT_SHEET};

    fn build_wb() -> Workbook {
        let doc = new_document_id();
        let mut wb = Workbook::new(&doc);
        wb.apply_ops(&[
            CellOp::new(
                0,
                "sys",
                OpKind::CreateDocument {
                    name: "导出".into(),
                    owner: "alice".into(),
                },
            ),
            CellOp::new(
                1,
                "sys",
                OpKind::AddSheet {
                    sheet: DEFAULT_SHEET.into(),
                    position: None,
                },
            ),
        ]);
        wb
    }

    #[test]
    fn xlsx_roundtrip_with_formula() {
        let mut wb = build_wb();
        wb.apply_ops(&[
            cell_set(2, "A1", Some("1"), None),
            cell_set(3, "A2", Some("2"), None),
            cell_set(4, "A3", None, Some("=SUM(A1:A2)")),
            cell_set(5, "B1", Some("标签"), None),
        ]);
        let bytes = workbook_to_xlsx(&wb).unwrap();
        assert_eq!(&bytes[..2], b"PK", "xlsx 是 zip 容器");

        let sheets = xlsx_to_sheets(&bytes).unwrap();
        assert_eq!(sheets.len(), 1);
        assert_eq!(sheets[0].name, DEFAULT_SHEET);
        let a3 = sheets[0].cells.iter().find(|c| c.row == 3 && c.col == 1);
        assert!(a3.is_some(), "A3 应存在");
        assert_eq!(a3.unwrap().formula.as_deref(), Some("=SUM(A1:A2)"));
        let b1 = sheets[0].cells.iter().find(|c| c.row == 1 && c.col == 2);
        assert_eq!(b1.unwrap().value.as_deref(), Some("标签"));
    }

    #[test]
    fn xlsx_preserves_bold_style() {
        let mut wb = build_wb();
        wb.apply_ops(&[CellOp::new(
            2,
            "sys",
            OpKind::SetCell {
                sheet: DEFAULT_SHEET.into(),
                row: 1,
                col: 1,
                value: Some("标题".into()),
                formula: None,
                style: Some(CellStyle {
                    bold: true,
                    color: Some("#FF0000".into()),
                    bg: None,
                    align: Some("center".into()),
                }),
            },
        )]);
        let bytes = workbook_to_xlsx(&wb).unwrap();
        // 读回来值不丢(样式经 xlsx Format 转换,MVP 只验证值与能解析)
        let sheets = xlsx_to_sheets(&bytes).unwrap();
        assert_eq!(sheets[0].cells[0].value.as_deref(), Some("标题"));
    }

    #[test]
    fn csv_roundtrip() {
        let grid = vec![
            vec!["名称".into(), "数量".into()],
            vec!["苹果".into(), "3".into()],
            vec!["带,逗号".into(), "-2.5".into()],
        ];
        let csv = grid_to_csv(&grid);
        assert!(csv.contains("\"带,逗号\""), "含逗号字段要加引号");
        let back = csv_to_grid(&csv).unwrap();
        assert_eq!(back, grid);
    }

    #[test]
    fn workbook_sheet_to_csv_uses_computed() {
        let mut wb = build_wb();
        wb.apply_ops(&[
            cell_set(2, "A1", Some("4"), None),
            cell_set(3, "A2", Some("6"), None),
            cell_set(4, "A3", None, Some("=A1*A2")),
        ]);
        let csv = workbook_sheet_to_csv(&wb, DEFAULT_SHEET).unwrap();
        assert!(csv.contains("24"), "公式应导出为计算值 24,实际: {csv}");
        assert!(workbook_sheet_to_csv(&wb, "Nope").is_err());
    }

    fn cell_set(ts: i64, a1: &str, value: Option<&str>, formula: Option<&str>) -> CellOp {
        let (row, col) = parse_a1(a1);
        CellOp::new(
            ts,
            "sys",
            OpKind::SetCell {
                sheet: DEFAULT_SHEET.into(),
                row,
                col,
                value: value.map(|s| s.into()),
                formula: formula.map(|s| s.into()),
                style: None,
            },
        )
    }

    fn parse_a1(a1: &str) -> (u32, u32) {
        let split = a1.find(|c: char| c.is_ascii_digit()).unwrap();
        let (letters, digits) = a1.split_at(split);
        let mut col = 0u32;
        for ch in letters.chars() {
            col = col * 26 + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
        }
        (digits.parse().unwrap(), col)
    }
}
