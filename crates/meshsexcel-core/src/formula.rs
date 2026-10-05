//! Excel 风格公式引擎:解析(`parse`)、求值(`evaluate`)、引用提取(`extract_refs`)。
//!
//! 支持:算术/比较/连接运算符、单元格引用 `A1`、跨表引用 `Sheet2!B3`、
//! 区域 `A1:B3`、以及 SUM/AVERAGE/MIN/MAX/COUNT/COUNTA/IF/AND/OR/NOT/
//! ABS/ROUND/CONCAT 函数。错误以 `Value::Error`(#DIV/0! 等)传播,
//! 与 Excel 习惯一致。

use crate::ops::{col_name, parse_col_name};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

/// 单个单元格可取到的值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Value {
    Number(f64),
    Text(String),
    Bool(bool),
    Error(String),
    Empty,
}

impl Value {
    pub fn err_div0() -> Self {
        Value::Error("#DIV/0!".into())
    }
    pub fn err_value() -> Self {
        Value::Error("#VALUE!".into())
    }
    pub fn err_name() -> Self {
        Value::Error("#NAME?".into())
    }
    pub fn err_ref() -> Self {
        Value::Error("#REF!".into())
    }
    pub fn err_circ() -> Self {
        Value::Error("#CIRC!".into())
    }

    /// UI / 导出用的显示文本。
    pub fn display(&self) -> String {
        match self {
            Value::Number(n) => format_number(*n),
            Value::Text(s) => s.clone(),
            Value::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).into(),
            Value::Error(e) => e.clone(),
            Value::Empty => String::new(),
        }
    }

    /// 算术运算的数值化(Excel 语义:空→0,TRUE→1,纯数字文本可转)。
    fn to_number(&self) -> Result<f64, Value> {
        match self {
            Value::Number(n) => Ok(*n),
            Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Value::Empty => Ok(0.0),
            Value::Text(s) => s.trim().parse::<f64>().map_err(|_| Value::err_value()),
            Value::Error(e) => Err(Value::Error(e.clone())),
        }
    }

    /// 连接运算的文本化。
    fn to_text(&self) -> Result<String, Value> {
        match self {
            Value::Error(e) => Err(Value::Error(e.clone())),
            other => Ok(other.display()),
        }
    }

    /// 条件真值:数字非零、TRUE;空为 FALSE;文本一律 #VALUE!。
    fn to_bool(&self) -> Result<bool, Value> {
        match self {
            Value::Bool(b) => Ok(*b),
            Value::Number(n) => Ok(*n != 0.0),
            Value::Empty => Ok(false),
            Value::Text(_) => Err(Value::err_value()),
            Value::Error(e) => Err(Value::Error(e.clone())),
        }
    }

    /// 比较排序用的类型秩:数字 < 文本 < 布尔。
    fn type_rank(&self) -> u8 {
        match self {
            Value::Number(_) | Value::Empty => 0,
            Value::Text(_) => 1,
            Value::Bool(_) => 2,
            Value::Error(_) => 3,
        }
    }
}

/// 数字显示:整数不带小数点;过大/过小用科学计数法。
pub fn format_number(n: f64) -> String {
    if !n.is_finite() {
        return if n.is_nan() {
            "#NUM!".into()
        } else {
            "#DIV/0!".into()
        };
    }
    if n == n.trunc() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else if n.abs() >= 1e15 || (n != 0.0 && n.abs() < 1e-9) {
        format!("{:e}", n)
    } else {
        format!("{}", n)
    }
}

/// 求值上下文:公式引擎只通过该接口读单元格,便于测试与解耦。
pub trait FormulaContext {
    fn cell_value(&self, sheet: &str, row: u32, col: u32) -> Value;
    fn sheet_exists(&self, sheet: &str) -> bool;
}

/// 单元格引用。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CellRef {
    pub sheet: Option<String>,
    pub row: u32,
    pub col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

/// 公式语法树。
#[derive(Debug, Clone)]
pub enum Ast {
    Num(f64),
    Str(String),
    Bool(bool),
    Ref(CellRef),
    Range {
        sheet: Option<String>,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
    },
    Fn(String, Vec<Ast>),
    Neg(Box<Ast>),
    Percent(Box<Ast>),
    Bin(BinOp, Box<Ast>, Box<Ast>),
}

impl fmt::Display for Ast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ast::Num(n) => write!(f, "{}", format_number(*n)),
            Ast::Str(s) => write!(f, "\"{}\"", s.replace('"', "\"\"")),
            Ast::Bool(b) => write!(f, "{}", if *b { "TRUE" } else { "FALSE" }),
            Ast::Ref(r) => write!(f, "{}{}{}", sheet_prefix(&r.sheet), col_name(r.col), r.row),
            Ast::Range {
                sheet,
                r1,
                c1,
                r2,
                c2,
            } => write!(
                f,
                "{}{}{}:{}{}",
                sheet_prefix(sheet),
                col_name(*c1),
                r1,
                col_name(*c2),
                r2
            ),
            Ast::Fn(name, args) => {
                write!(f, "{name}(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{a}")?;
                }
                write!(f, ")")
            }
            Ast::Neg(a) => write!(f, "-{a}"),
            Ast::Percent(a) => write!(f, "({a})%"),
            Ast::Bin(op, l, r) => write!(f, "({l} {op:?} {r})"),
        }
    }
}

fn sheet_prefix(sheet: &Option<String>) -> String {
    match sheet {
        Some(s) if s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => format!("{s}!"),
        Some(s) => format!("'{}'!", s.replace('\'', "''")),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// 词法
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ref(CellRef),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Amp,
    Percent,
    LParen,
    RParen,
    Comma,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Colon,
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
}

/// 把形如 `=SUM(A1:B2, 3)` 的公式切开(可不带前导 `=`)。
fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let mut lx = Lexer {
        src: src.as_bytes(),
        pos: 0,
    };
    let mut out = Vec::new();
    while lx.pos < lx.src.len() {
        match lx.src[lx.pos] {
            b' ' | b'\t' | b'\r' | b'\n' => lx.pos += 1,
            _ => out.push(lx.next_token()?),
        }
    }
    Ok(out)
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn push_utf8_char(&mut self, into: &mut String) -> Result<(), String> {
        let rest = &self.src[self.pos..];
        let ch_len = utf8_len(rest[0]).min(rest.len());
        let chunk = std::str::from_utf8(&rest[..ch_len]).map_err(|_| "bad utf8".to_string())?;
        into.push_str(chunk);
        self.pos += ch_len;
        Ok(())
    }

    fn next_token(&mut self) -> Result<Tok, String> {
        let b = self.peek().ok_or_else(|| "unexpected end".to_string())?;
        self.pos += 1;
        Ok(match b {
            b'+' => Tok::Plus,
            b'-' => Tok::Minus,
            b'*' => Tok::Star,
            b'/' => Tok::Slash,
            b'^' => Tok::Caret,
            b'&' => Tok::Amp,
            b'%' => Tok::Percent,
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b',' => Tok::Comma,
            b':' => Tok::Colon,
            b'=' => Tok::Eq,
            b'<' => {
                if self.peek() == Some(b'>') {
                    self.pos += 1;
                    Tok::Ne
                } else if self.peek() == Some(b'=') {
                    self.pos += 1;
                    Tok::Le
                } else {
                    Tok::Lt
                }
            }
            b'>' => {
                if self.peek() == Some(b'=') {
                    self.pos += 1;
                    Tok::Ge
                } else {
                    Tok::Gt
                }
            }
            b'"' => {
                let mut s = String::new();
                loop {
                    match self.peek() {
                        None => return Err("unterminated string".into()),
                        Some(b'"') => {
                            self.pos += 1;
                            if self.peek() == Some(b'"') {
                                s.push('"');
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                        Some(_) => self.push_utf8_char(&mut s)?,
                    }
                }
                Tok::Str(s)
            }
            b'\'' => self.lex_quoted_sheet_ref()?,
            b'$' | b'0'..=b'9' => {
                self.pos -= 1;
                if b == b'$' {
                    let (row, col) = {
                        self.pos += 1;
                        self.lex_a1()?
                    };
                    Tok::Ref(CellRef {
                        sheet: None,
                        row,
                        col,
                    })
                } else {
                    self.lex_number()?
                }
            }
            _ if b.is_ascii_alphabetic() || b == b'_' => {
                self.pos -= 1;
                self.lex_ident_or_ref()?
            }
            _ => return Err(format!("unexpected character: {}", b as char)),
        })
    }

    /// 'My Sheet'!A1(带引号工作表名)。
    fn lex_quoted_sheet_ref(&mut self) -> Result<Tok, String> {
        // 调用时开引号已被消费
        let mut name = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated sheet name".into()),
                Some(b'\'') => {
                    self.pos += 1;
                    if self.peek() == Some(b'\'') {
                        name.push('\'');
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                Some(_) => self.push_utf8_char(&mut name)?,
            }
        }
        if self.peek() != Some(b'!') {
            return Err("expected ! after sheet name".into());
        }
        self.pos += 1;
        let (row, col) = self.lex_a1()?;
        Ok(Tok::Ref(CellRef {
            sheet: Some(name),
            row,
            col,
        }))
    }

    fn lex_number(&mut self) -> Result<Tok, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9') | Some(b'.')) {
            self.pos += 1;
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos = save;
            } else {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).map_err(|_| "bad number")?;
        text.parse::<f64>()
            .map(Tok::Num)
            .map_err(|_| format!("bad number: {text}"))
    }

    /// A1(含 $A$1);调用时 self.pos 停在字母或 $ 上。
    fn lex_a1(&mut self) -> Result<(u32, u32), String> {
        while self.peek() == Some(b'$') {
            self.pos += 1;
        }
        let col_start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        let col_text = std::str::from_utf8(&self.src[col_start..self.pos])
            .map_err(|_| "bad col")?
            .to_ascii_uppercase();
        let col = parse_col_name(&col_text).ok_or_else(|| "bad column name".to_string())?;
        while self.peek() == Some(b'$') {
            self.pos += 1;
        }
        let row_start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        let row_text =
            std::str::from_utf8(&self.src[row_start..self.pos]).map_err(|_| "bad row")?;
        let row: u32 = row_text
            .parse()
            .map_err(|_| format!("bad row: {row_text}"))?;
        if row == 0 {
            return Err("row must be >= 1".into());
        }
        Ok((row, col))
    }

    /// 字母开头:可能是函数名、TRUE/FALSE、A1 引用或 Sheet!A1 引用。
    fn lex_ident_or_ref(&mut self) -> Result<Tok, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'.')
        {
            self.pos += 1;
        }
        let word = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|_| "bad ident".to_string())?
            .to_string();

        // Sheet!A1:后跟 ! 即工作表前缀
        if self.peek() == Some(b'!') {
            self.pos += 1;
            let (row, col) = self.lex_a1()?;
            return Ok(Tok::Ref(CellRef {
                sheet: Some(word),
                row,
                col,
            }));
        }

        // 裸 A1 引用:字母段 + 数字段,且后面不是 "(" (函数调用)
        let is_func_call = self.peek() == Some(b'(');
        let letters = word.chars().take_while(|c| c.is_ascii_alphabetic()).count();
        let digits = word.chars().skip(letters).count();
        if !is_func_call
            && letters > 0
            && digits > 0
            && letters + digits == word.len()
            && word.chars().all(|c| c.is_ascii_alphanumeric())
        {
            if let (Some(col), Ok(row)) =
                (parse_col_name(&word[..letters]), word[letters..].parse())
            {
                if row >= 1 {
                    return Ok(Tok::Ref(CellRef {
                        sheet: None,
                        row,
                        col,
                    }));
                }
            }
        }

        Ok(Tok::Ident(word))
    }
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

// ---------------------------------------------------------------------------
// 语法
// ---------------------------------------------------------------------------

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: u32,
}

const MAX_DEPTH: u32 = 128;

/// 解析公式体(可含前导 `=`),语法错误返回 Err。
pub fn parse(src: &str) -> Result<Ast, String> {
    let body = src.trim().trim_start_matches('=');
    if body.trim().is_empty() {
        return Err("empty formula".into());
    }
    let toks = lex(body)?;
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let ast = p.parse_comparison()?;
    if p.pos != p.toks.len() {
        return Err("trailing tokens".into());
    }
    Ok(ast)
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("formula too deeply nested".into());
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn parse_comparison(&mut self) -> Result<Ast, String> {
        self.enter()?;
        let mut left = self.parse_concat()?;
        while let Some(op) = match self.peek() {
            Some(Tok::Eq) => Some(BinOp::Eq),
            Some(Tok::Ne) => Some(BinOp::Ne),
            Some(Tok::Lt) => Some(BinOp::Lt),
            Some(Tok::Gt) => Some(BinOp::Gt),
            Some(Tok::Le) => Some(BinOp::Le),
            Some(Tok::Ge) => Some(BinOp::Ge),
            _ => None,
        } {
            self.pos += 1;
            let right = self.parse_concat()?;
            left = Ast::Bin(op, Box::new(left), Box::new(right));
        }
        self.leave();
        Ok(left)
    }

    fn parse_concat(&mut self) -> Result<Ast, String> {
        self.enter()?;
        let mut left = self.parse_additive()?;
        while self.peek() == Some(&Tok::Amp) {
            self.pos += 1;
            let right = self.parse_additive()?;
            left = Ast::Bin(BinOp::Concat, Box::new(left), Box::new(right));
        }
        self.leave();
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Ast, String> {
        self.enter()?;
        let mut left = self.parse_multiplicative()?;
        while let Some(op) = match self.peek() {
            Some(Tok::Plus) => Some(BinOp::Add),
            Some(Tok::Minus) => Some(BinOp::Sub),
            _ => None,
        } {
            self.pos += 1;
            let right = self.parse_multiplicative()?;
            left = Ast::Bin(op, Box::new(left), Box::new(right));
        }
        self.leave();
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Ast, String> {
        self.enter()?;
        let mut left = self.parse_power()?;
        while let Some(op) = match self.peek() {
            Some(Tok::Star) => Some(BinOp::Mul),
            Some(Tok::Slash) => Some(BinOp::Div),
            _ => None,
        } {
            self.pos += 1;
            let right = self.parse_power()?;
            left = Ast::Bin(op, Box::new(left), Box::new(right));
        }
        self.leave();
        Ok(left)
    }

    fn parse_power(&mut self) -> Result<Ast, String> {
        self.enter()?;
        let base = self.parse_unary()?;
        if self.peek() == Some(&Tok::Caret) {
            self.pos += 1;
            let exp = self.parse_power()?; // 右结合
            self.leave();
            return Ok(Ast::Bin(BinOp::Pow, Box::new(base), Box::new(exp)));
        }
        self.leave();
        Ok(base)
    }

    fn parse_unary(&mut self) -> Result<Ast, String> {
        self.enter()?;
        let mut neg = false;
        while matches!(self.peek(), Some(Tok::Plus) | Some(Tok::Minus)) {
            if self.peek() == Some(&Tok::Minus) {
                neg = !neg;
            }
            self.pos += 1;
        }
        let inner = self.parse_postfix()?;
        self.leave();
        Ok(if neg {
            Ast::Neg(Box::new(inner))
        } else {
            inner
        })
    }

    fn parse_postfix(&mut self) -> Result<Ast, String> {
        self.enter()?;
        let mut node = self.parse_primary()?;
        while self.peek() == Some(&Tok::Percent) {
            self.pos += 1;
            node = Ast::Percent(Box::new(node));
        }
        self.leave();
        Ok(node)
    }

    fn parse_primary(&mut self) -> Result<Ast, String> {
        let tok = self
            .toks
            .get(self.pos)
            .cloned()
            .ok_or_else(|| "unexpected end of formula".to_string())?;
        self.pos += 1;
        match tok {
            Tok::Num(n) => Ok(Ast::Num(n)),
            Tok::Str(s) => Ok(Ast::Str(s)),
            Tok::Ref(r) => {
                if self.peek() == Some(&Tok::Colon) {
                    self.pos += 1;
                    match self.toks.get(self.pos).cloned() {
                        Some(Tok::Ref(r2)) => {
                            self.pos += 1;
                            Ok(Ast::Range {
                                sheet: r.sheet.or(r2.sheet),
                                r1: r.row.min(r2.row),
                                c1: r.col.min(r2.col),
                                r2: r.row.max(r2.row),
                                c2: r.col.max(r2.col),
                            })
                        }
                        _ => Err("expected cell after :".into()),
                    }
                } else {
                    Ok(Ast::Ref(r))
                }
            }
            Tok::Ident(name) => {
                if self.peek() == Some(&Tok::LParen) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if self.peek() != Some(&Tok::RParen) {
                        loop {
                            args.push(self.parse_comparison()?);
                            if self.peek() == Some(&Tok::Comma) {
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    if self.peek() != Some(&Tok::RParen) {
                        return Err("expected )".into());
                    }
                    self.pos += 1;
                    Ok(Ast::Fn(name, args))
                } else if name.eq_ignore_ascii_case("TRUE") {
                    Ok(Ast::Bool(true))
                } else if name.eq_ignore_ascii_case("FALSE") {
                    Ok(Ast::Bool(false))
                } else {
                    Err(format!("unknown name: {name}"))
                }
            }
            Tok::LParen => {
                let inner = self.parse_comparison()?;
                if self.peek() != Some(&Tok::RParen) {
                    return Err("expected )".into());
                }
                self.pos += 1;
                Ok(inner)
            }
            t => Err(format!("unexpected token: {t:?}")),
        }
    }
}

// ---------------------------------------------------------------------------
// 求值
// ---------------------------------------------------------------------------

/// 区域展开上限,防止 A1:XFD1048576 之类的引用拖垮依赖图。
pub const MAX_RANGE_CELLS: u64 = 262_144;

/// 求值一个已解析的公式。`cur_sheet` 是公式所在表(裸引用的默认表)。
pub fn evaluate(ast: &Ast, cur_sheet: &str, ctx: &dyn FormulaContext) -> Value {
    match eval(ast, cur_sheet, ctx) {
        Ok(v) => v,
        Err(e) => e,
    }
}

/// 便捷入口:解析并求值。
pub fn evaluate_formula(expr: &str, cur_sheet: &str, ctx: &dyn FormulaContext) -> Value {
    match parse(expr) {
        Ok(ast) => evaluate(&ast, cur_sheet, ctx),
        Err(_) => Value::err_name(),
    }
}

fn eval(ast: &Ast, sheet: &str, ctx: &dyn FormulaContext) -> Result<Value, Value> {
    match ast {
        Ast::Num(n) => Ok(Value::Number(*n)),
        Ast::Str(s) => Ok(Value::Text(s.clone())),
        Ast::Bool(b) => Ok(Value::Bool(*b)),
        Ast::Ref(r) => resolve_ref(r, sheet, ctx),
        Ast::Range { .. } => Err(Value::err_value()), // 裸区域不能当标量
        Ast::Fn(name, args) => eval_fn(name, args, sheet, ctx),
        Ast::Neg(inner) => Ok(Value::Number(-eval(inner, sheet, ctx)?.to_number()?)),
        Ast::Percent(inner) => Ok(Value::Number(eval(inner, sheet, ctx)?.to_number()? / 100.0)),
        Ast::Bin(op, l, r) => {
            let lv = eval(l, sheet, ctx)?;
            let rv = eval(r, sheet, ctx)?;
            apply_bin(*op, lv, rv)
        }
    }
}

fn resolve_ref(r: &CellRef, cur: &str, ctx: &dyn FormulaContext) -> Result<Value, Value> {
    let target_sheet = r.sheet.as_deref().unwrap_or(cur);
    if !ctx.sheet_exists(target_sheet) {
        return Err(Value::err_ref());
    }
    Ok(ctx.cell_value(target_sheet, r.row, r.col))
}

fn apply_bin(op: BinOp, lv: Value, rv: Value) -> Result<Value, Value> {
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow => {
            let a = lv.to_number()?;
            let b = rv.to_number()?;
            let v = match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                BinOp::Div => {
                    if b == 0.0 {
                        return Err(Value::err_div0());
                    }
                    a / b
                }
                BinOp::Pow => a.powf(b),
                _ => unreachable!(),
            };
            Ok(Value::Number(v))
        }
        BinOp::Concat => {
            let a = lv.to_text()?;
            let b = rv.to_text()?;
            Ok(Value::Text(format!("{a}{b}")))
        }
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
            let ord = compare_values(&lv, &rv)?;
            let b = match op {
                BinOp::Eq => ord == std::cmp::Ordering::Equal,
                BinOp::Ne => ord != std::cmp::Ordering::Equal,
                BinOp::Lt => ord == std::cmp::Ordering::Less,
                BinOp::Gt => ord == std::cmp::Ordering::Greater,
                BinOp::Le => ord != std::cmp::Ordering::Greater,
                BinOp::Ge => ord != std::cmp::Ordering::Less,
                _ => unreachable!(),
            };
            Ok(Value::Bool(b))
        }
    }
}

/// Excel 风格比较:同型直接比(文本不区分大小写);跨型按 数字<文本<布尔;
/// 空值按另一侧类型归一(对数字是 0,对文本是 "")。
fn compare_values(l: &Value, r: &Value) -> Result<std::cmp::Ordering, Value> {
    if let Value::Error(e) = l {
        return Err(Value::Error(e.clone()));
    }
    if let Value::Error(e) = r {
        return Err(Value::Error(e.clone()));
    }
    let norm_l = match (l, r) {
        (Value::Empty, Value::Text(_)) => Value::Text(String::new()),
        (Value::Empty, _) => Value::Number(0.0),
        _ => l.clone(),
    };
    let norm_r = match (l, r) {
        (Value::Text(_), Value::Empty) => Value::Text(String::new()),
        (_, Value::Empty) => Value::Number(0.0),
        _ => r.clone(),
    };
    let (lr, rr) = (norm_l.type_rank(), norm_r.type_rank());
    if lr != rr {
        return Ok(lr.cmp(&rr));
    }
    Ok(match (&norm_l, &norm_r) {
        (Value::Text(a), Value::Text(b)) => a.to_lowercase().cmp(&b.to_lowercase()),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => {
            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
        }
        _ => std::cmp::Ordering::Equal,
    })
}

fn eval_fn(
    name: &str,
    args: &[Ast],
    sheet: &str,
    ctx: &dyn FormulaContext,
) -> Result<Value, Value> {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "IF" => {
            let [cond, t, f] = args else {
                return Err(Value::err_value());
            };
            if eval(cond, sheet, ctx)?.to_bool()? {
                eval(t, sheet, ctx)
            } else {
                eval(f, sheet, ctx)
            }
        }
        "AND" | "OR" => {
            if args.is_empty() {
                return Err(Value::err_value());
            }
            let is_or = upper == "OR";
            let mut acc = !is_or;
            for a in args {
                let v = eval(a, sheet, ctx)?.to_bool()?;
                acc = if is_or { acc || v } else { acc && v };
            }
            Ok(Value::Bool(acc))
        }
        "NOT" => {
            let [a] = args else {
                return Err(Value::err_value());
            };
            Ok(Value::Bool(!eval(a, sheet, ctx)?.to_bool()?))
        }
        "ABS" => {
            let [a] = args else {
                return Err(Value::err_value());
            };
            Ok(Value::Number(eval(a, sheet, ctx)?.to_number()?.abs()))
        }
        "ROUND" => {
            if args.is_empty() || args.len() > 2 {
                return Err(Value::err_value());
            }
            let x = eval(&args[0], sheet, ctx)?.to_number()?;
            let digits = if args.len() == 2 {
                eval(&args[1], sheet, ctx)?.to_number()?
            } else {
                0.0
            };
            let factor = 10f64.powf(digits);
            // Excel 是"四舍五入远离零"
            let scaled = x * factor;
            let rounded = if scaled >= 0.0 {
                (scaled + 0.5).floor()
            } else {
                (scaled - 0.5).ceil()
            };
            Ok(Value::Number(rounded / factor))
        }
        "SUM" | "AVERAGE" | "MIN" | "MAX" | "COUNT" | "COUNTA" => {
            let (mut sum, mut count, mut counta, mut min, mut max) =
                (0.0, 0u64, 0u64, f64::INFINITY, f64::NEG_INFINITY);
            for a in args {
                for v in flatten_arg(a, sheet, ctx)? {
                    match v {
                        Value::Number(n) => {
                            sum += n;
                            count += 1;
                            counta += 1; // COUNTA 也计数字
                            min = min.min(n);
                            max = max.max(n);
                        }
                        Value::Empty => {}
                        Value::Error(e) => {
                            if upper == "COUNT" || upper == "COUNTA" {
                                counta += 1; // COUNTA 计错误值
                            } else {
                                return Err(Value::Error(e));
                            }
                        }
                        _ => {
                            counta += 1; // 文本/布尔:仅 COUNTA 计数
                        }
                    }
                }
            }
            Ok(match upper.as_str() {
                "SUM" => Value::Number(sum),
                "AVERAGE" => {
                    if count == 0 {
                        Value::err_div0()
                    } else {
                        Value::Number(sum / count as f64)
                    }
                }
                "MIN" => {
                    if count == 0 {
                        Value::Number(0.0)
                    } else {
                        Value::Number(min)
                    }
                }
                "MAX" => {
                    if count == 0 {
                        Value::Number(0.0)
                    } else {
                        Value::Number(max)
                    }
                }
                "COUNT" => Value::Number(count as f64),
                _ => Value::Number(counta as f64), // COUNTA
            })
        }
        "CONCAT" | "CONCATENATE" => {
            let mut out = String::new();
            for a in args {
                for v in flatten_arg(a, sheet, ctx)? {
                    out.push_str(&v.to_text()?);
                }
            }
            Ok(Value::Text(out))
        }
        _ => Err(Value::err_name()),
    }
}

/// 把参数展平成值序列:标量 → 单值;区域 → 逐格(带上限)。
fn flatten_arg(a: &Ast, sheet: &str, ctx: &dyn FormulaContext) -> Result<Vec<Value>, Value> {
    match a {
        Ast::Range {
            sheet: rs,
            r1,
            c1,
            r2,
            c2,
        } => {
            let target = rs.as_deref().unwrap_or(sheet);
            if !ctx.sheet_exists(target) {
                return Err(Value::err_ref());
            }
            let cols = (*c2 as u64).saturating_sub(*c1 as u64) + 1;
            let rows = (*r2 as u64).saturating_sub(*r1 as u64) + 1;
            if rows.saturating_mul(cols) > MAX_RANGE_CELLS {
                return Err(Value::Error("#RANGE!".into()));
            }
            let mut out = Vec::with_capacity((rows * cols) as usize);
            for row in *r1..=*r2 {
                for col in *c1..=*c2 {
                    out.push(ctx.cell_value(target, row, col));
                }
            }
            Ok(out)
        }
        other => Ok(vec![eval(other, sheet, ctx)?]),
    }
}

// ---------------------------------------------------------------------------
// 引用提取(依赖图用)
// ---------------------------------------------------------------------------

/// 从语法树提取全部被引用的单元格(区域展开,受 MAX_RANGE_CELLS 限制)。
pub fn extract_refs(ast: &Ast) -> HashSet<(Option<String>, u32, u32)> {
    let mut out = HashSet::new();
    walk(ast, &mut out);
    out
}

fn walk(ast: &Ast, out: &mut HashSet<(Option<String>, u32, u32)>) {
    match ast {
        Ast::Ref(r) => {
            out.insert((r.sheet.clone(), r.row, r.col));
        }
        Ast::Range {
            sheet,
            r1,
            c1,
            r2,
            c2,
        } => {
            let cols = (*c2 as u64).saturating_sub(*c1 as u64) + 1;
            let rows = (*r2 as u64).saturating_sub(*r1 as u64) + 1;
            if rows.saturating_mul(cols) <= MAX_RANGE_CELLS {
                for row in *r1..=*r2 {
                    for col in *c1..=*c2 {
                        out.insert((sheet.clone(), row, col));
                    }
                }
            }
        }
        Ast::Neg(a) | Ast::Percent(a) => walk(a, out),
        Ast::Bin(_, a, b) => {
            walk(a, out);
            walk(b, out);
        }
        Ast::Fn(_, args) => {
            for a in args {
                walk(a, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// 空上下文(测试/无网格求值用)
// ---------------------------------------------------------------------------

/// 恒空的上下文:所有单元格都是 Empty。
pub struct EmptyContext;

impl FormulaContext for EmptyContext {
    fn cell_value(&self, _sheet: &str, _row: u32, _col: u32) -> Value {
        Value::Empty
    }
    fn sheet_exists(&self, _sheet: &str) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Ctx {
        cells: HashMap<(String, u32, u32), Value>,
        sheets: Vec<String>,
    }

    impl Ctx {
        fn new(sheets: &[&str]) -> Self {
            Self {
                cells: HashMap::new(),
                sheets: sheets.iter().map(|s| s.to_string()).collect(),
            }
        }
        fn set(&mut self, sheet: &str, row: u32, col: u32, v: Value) {
            self.cells.insert((sheet.to_string(), row, col), v);
        }
    }

    impl FormulaContext for Ctx {
        fn cell_value(&self, sheet: &str, row: u32, col: u32) -> Value {
            self.cells
                .get(&(sheet.to_string(), row, col))
                .cloned()
                .unwrap_or(Value::Empty)
        }
        fn sheet_exists(&self, sheet: &str) -> bool {
            self.sheets.iter().any(|s| s == sheet)
        }
    }

    fn ev(expr: &str) -> Value {
        evaluate_formula(expr, "Sheet1", &EmptyContext)
    }

    fn ev_ctx(expr: &str, ctx: &Ctx) -> Value {
        evaluate_formula(expr, "Sheet1", ctx)
    }

    #[test]
    fn arithmetic() {
        assert_eq!(ev("=1+2*3"), Value::Number(7.0));
        assert_eq!(ev("=(1+2)*3"), Value::Number(9.0));
        assert_eq!(ev("=2^3^2"), Value::Number(512.0), "幂右结合");
        assert_eq!(ev("=-2^2"), Value::Number(4.0), "Excel: 一元负号先于幂");
        assert_eq!(ev("=50%"), Value::Number(0.5));
        assert_eq!(ev("=10/4"), Value::Number(2.5));
        assert_eq!(ev("=1/0"), Value::err_div0());
    }

    #[test]
    fn concat_and_compare() {
        assert_eq!(ev("=\"a\"&\"b\"&1"), Value::Text("ab1".into()));
        assert_eq!(
            ev("=\"abc\"=\"ABC\""),
            Value::Bool(true),
            "文本比较不区分大小写"
        );
        assert_eq!(ev("=2>1"), Value::Bool(true));
        assert_eq!(ev("=1<>2"), Value::Bool(true));
        assert_eq!(ev("=\"b\">\"a\""), Value::Bool(true));
        assert_eq!(ev("=1>\"a\""), Value::Bool(false), "跨型:数字 < 文本");
    }

    #[test]
    fn refs_and_sheets() {
        let mut ctx = Ctx::new(&["Sheet1", "Data"]);
        ctx.set("Sheet1", 1, 1, Value::Number(10.0));
        ctx.set("Data", 2, 2, Value::Number(5.0));
        assert_eq!(ev_ctx("=A1+1", &ctx), Value::Number(11.0));
        assert_eq!(ev_ctx("=Data!B2*2", &ctx), Value::Number(10.0));
        ctx.set("Sheet1", 1, 2, Value::Text("x".into()));
        assert_eq!(ev_ctx("=B1&\"y\"", &ctx), Value::Text("xy".into()));
        assert_eq!(
            ev_ctx("=Missing!A1", &ctx),
            Value::err_ref(),
            "不存在的表 → #REF!"
        );
        ctx.set("Sheet1", 5, 1, Value::Number(7.0));
        ctx.set("Sheet1", 6, 1, Value::Number(8.0));
        assert_eq!(ev_ctx("=SUM(Data!B2:A5)", &ctx), Value::Number(5.0));
        assert_eq!(ev_ctx("=SUM(A5:A6)", &ctx), Value::Number(15.0));
    }

    #[test]
    fn aggregates() {
        let mut ctx = Ctx::new(&["Sheet1"]);
        ctx.set("Sheet1", 1, 1, Value::Number(1.0));
        ctx.set("Sheet1", 2, 1, Value::Number(2.0));
        ctx.set("Sheet1", 3, 1, Value::Number(3.0));
        ctx.set("Sheet1", 4, 1, Value::Text("skip".into()));
        assert_eq!(
            ev_ctx("=SUM(A1:A4)", &ctx),
            Value::Number(6.0),
            "区域内文本不计入 SUM"
        );
        assert_eq!(ev_ctx("=AVERAGE(A1:A3)", &ctx), Value::Number(2.0));
        assert_eq!(ev_ctx("=MIN(A1:A4)", &ctx), Value::Number(1.0));
        assert_eq!(ev_ctx("=MAX(A1:A4)", &ctx), Value::Number(3.0));
        assert_eq!(ev_ctx("=COUNT(A1:A4)", &ctx), Value::Number(3.0));
        assert_eq!(ev_ctx("=COUNTA(A1:A4)", &ctx), Value::Number(4.0));
        assert_eq!(ev_ctx("=SUM(A1:A3,10)", &ctx), Value::Number(16.0));
    }

    #[test]
    fn logic_and_rounding() {
        assert_eq!(ev("=IF(1>0,\"yes\",\"no\")"), Value::Text("yes".into()));
        assert_eq!(ev("=IF(0,1,2)"), Value::Number(2.0));
        assert_eq!(ev("=AND(TRUE,1)"), Value::Bool(true));
        assert_eq!(ev("=OR(FALSE,0)"), Value::Bool(false));
        assert_eq!(ev("=NOT(FALSE)"), Value::Bool(true));
        assert_eq!(ev("=ROUND(2.5)"), Value::Number(3.0), "四舍五入远离零");
        assert_eq!(ev("=ROUND(-2.5)"), Value::Number(-3.0));
        assert_eq!(ev("=ROUND(123.456,2)"), Value::Number(123.46));
        assert_eq!(ev("=ABS(-4.2)"), Value::Number(4.2));
        assert_eq!(ev("=CONCAT(\"a\",1,TRUE)"), Value::Text("a1TRUE".into()));
    }

    #[test]
    fn errors_and_name() {
        assert_eq!(ev("=NOSUCHFN(1)"), Value::err_name());
        assert_eq!(ev("=\"abc\"+1"), Value::err_value());
        assert_eq!(ev("=1+NOSUCHFN(2)"), Value::err_name(), "错误沿运算传播");
        assert_eq!(ev("=SUM(BADFN(A1))"), Value::err_name());
        assert_eq!(
            ev("=IF(A1,1,2)"),
            Value::Number(2.0),
            "空单元格当条件按 FALSE"
        );
    }

    #[test]
    fn empty_coercion() {
        assert_eq!(ev("=Z99+5"), Value::Number(5.0), "空单元格按 0");
        assert_eq!(
            ev("=Z99&\"x\""),
            Value::Text("x".into()),
            "空单元格按空串连接"
        );
    }

    #[test]
    fn parse_failures() {
        assert!(parse("=SUM(").is_err());
        assert!(parse("=1++*").is_err());
        assert_eq!(
            evaluate_formula("=NOPE1", "Sheet1", &EmptyContext),
            Value::err_name(),
            "无法解析 → #NAME?"
        );
    }

    #[test]
    fn extract_refs_works() {
        let ast = parse("=SUM(A1:B2)+C3+Data!D4").unwrap();
        let refs = extract_refs(&ast);
        assert_eq!(refs.len(), 4 + 1 + 1); // A1:B2 展开 4 格 + C3 + Data!D4
        assert!(refs.contains(&(None, 1, 1)));
        assert!(refs.contains(&(None, 2, 2)));
        assert!(refs.contains(&(None, 3, 3)));
        assert!(refs.contains(&(Some("Data".into()), 4, 4)));
    }

    #[test]
    fn percent_and_neg_chain() {
        assert_eq!(ev("=--5"), Value::Number(5.0));
        assert_eq!(ev("=200%%"), Value::Number(0.02));
    }

    #[test]
    fn display_formatting() {
        assert_eq!(format_number(42.0), "42");
        assert_eq!(format_number(2.5), "2.5");
        assert_eq!(format_number(1e20), "1e20");
    }
}
