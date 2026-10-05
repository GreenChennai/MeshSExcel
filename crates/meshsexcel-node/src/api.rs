//! REST API:实现 `spec/openapi.yaml` 全部端点,另有标注 "扩展" 的补充端点
//! (cells 网格、ops 提交、导入、快照),供内嵌 Web UI 使用。

use crate::state::{AcceptOutcome, AppState};
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use meshsexcel_core::sheet::WorkbookGrid;
use meshsexcel_core::{Block, CellOp};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// 编译期内嵌 UI 资源(index.html / app.* / vendor/luckysheet/**)。
/// debug 构建直接读盘(改前端不用重编),release 构建打进二进制。
#[derive(RustEmbed)]
#[folder = "ui/"]
struct UiAssets;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(ui_index))
        .route("/sheet.html", get(ui_sheet))
        .route("/app.css", get(ui_css))
        .route("/app.js", get(ui_js))
        .route("/vendor/*path", get(ui_vendor))
        .route("/v1/node/info", get(node_info))
        .route("/v1/documents", get(list_documents).post(create_document))
        .route("/v1/documents/:doc_id", get(get_document))
        .route(
            "/v1/documents/:doc_id/blocks",
            get(list_blocks).post(submit_block),
        )
        .route("/v1/documents/:doc_id/ops", post(submit_ops))
        .route("/v1/documents/:doc_id/cells", get(get_cells))
        .route("/v1/documents/:doc_id/export/xlsx", get(export_xlsx))
        .route("/v1/documents/:doc_id/export/csv", get(export_csv))
        .route("/v1/documents/:doc_id/import/xlsx", post(import_xlsx))
        .route("/v1/documents/:doc_id/import/csv", post(import_csv))
        .route(
            "/v1/documents/:doc_id/snapshots",
            get(list_snapshots).post(create_snapshot),
        )
        .route("/v1/documents/:doc_id/restore", post(restore_snapshot))
        .route("/v1/nodes/peers", get(list_peers))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// 统一错误
// ---------------------------------------------------------------------------

struct ApiError(anyhow::Error, StatusCode);

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(e, StatusCode::INTERNAL_SERVER_ERROR)
    }
}

impl From<meshsexcel_core::Error> for ApiError {
    fn from(e: meshsexcel_core::Error) -> Self {
        ApiError(e.into(), StatusCode::INTERNAL_SERVER_ERROR)
    }
}

fn not_found(e: impl std::fmt::Display) -> ApiError {
    ApiError(anyhow::anyhow!("{e}"), StatusCode::NOT_FOUND)
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": format!("{}", self.0) });
        (self.1, Json(body)).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

// ---------------------------------------------------------------------------
// UI 静态资源(include! 编译期内嵌,单二进制零外部文件)
// ---------------------------------------------------------------------------

async fn ui_index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../ui/index.html"),
    )
}

async fn ui_sheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../ui/sheet.html"),
    )
}

async fn ui_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../ui/app.css"),
    )
}

async fn ui_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../ui/app.js"),
    )
}

/// vendor 静态资源(Luckysheet 等,经 rust-embed 内嵌)。
async fn ui_vendor(Path(path): Path<String>) -> Response {
    // 防目录穿越
    if path.split('/').any(|seg| seg == "..") {
        return StatusCode::NOT_FOUND.into_response();
    }
    // 路由捕获的是 /vendor/ 之后的部分,补全为 ui/ 内的完整键
    match UiAssets::get(&format!("vendor/{path}")) {
        Some(f) => ([(header::CONTENT_TYPE, mime_of(&path))], f.data).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "eot" => "application/vnd.ms-fontobject",
        "otf" => "font/otf",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// node
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct NodeInfo {
    node_id: String,
    pubkey: String,
    documents: usize,
    peers: usize,
}

async fn node_info(State(app): State<Arc<AppState>>) -> ApiResult<Json<NodeInfo>> {
    let docs = app.list_documents()?.len();
    let peers = app.list_peers()?.len();
    Ok(Json(NodeInfo {
        node_id: app.node_id.clone(),
        pubkey: app.verifying_key_hex(),
        documents: docs,
        peers,
    }))
}

/// openapi Peer:id/addr/last_seen。
#[derive(Serialize)]
struct PeerOut {
    id: String,
    addr: Option<String>,
    last_seen: Option<String>,
}

async fn list_peers(State(app): State<Arc<AppState>>) -> ApiResult<Json<Vec<PeerOut>>> {
    let peers = app
        .list_peers()?
        .into_iter()
        .map(|p| PeerOut {
            id: p.peer_id,
            addr: p.addr,
            last_seen: p.last_seen,
        })
        .collect();
    Ok(Json(peers))
}

// ---------------------------------------------------------------------------
// documents
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DocumentCreate {
    name: String,
    #[serde(default)]
    owner: String,
}

async fn create_document(
    State(app): State<Arc<AppState>>,
    Json(body): Json<DocumentCreate>,
) -> ApiResult<(StatusCode, Json<meshsexcel_core::store::DocumentSummary>)> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError(
            anyhow::anyhow!("document name is required"),
            StatusCode::BAD_REQUEST,
        ));
    }
    let owner = if body.owner.trim().is_empty() {
        app.node_id.clone()
    } else {
        body.owner.trim().to_string()
    };
    let doc = app.create_document(name, &owner)?;
    Ok((StatusCode::CREATED, Json(doc)))
}

async fn list_documents(
    State(app): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<meshsexcel_core::store::DocumentSummary>>> {
    Ok(Json(app.list_documents()?))
}

#[derive(Serialize)]
struct SheetOut {
    name: String,
    rows: u32,
    cols: u32,
}

#[derive(Serialize)]
struct DocumentOut {
    id: String,
    name: String,
    owner: Option<String>,
    head_block: Option<String>,
    created_at: Option<String>,
    sheets: Vec<SheetOut>,
}

async fn get_document(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
) -> ApiResult<Json<DocumentOut>> {
    let doc = app
        .get_document(&doc_id)?
        .ok_or_else(|| not_found(format!("document {doc_id}")))?;
    let grid = grid_of(&app, &doc_id)?;
    Ok(Json(DocumentOut {
        id: doc.id,
        name: doc.name,
        owner: doc.owner,
        head_block: doc.head_block,
        created_at: doc.created_at,
        sheets: grid
            .sheets
            .into_iter()
            .map(|s| SheetOut {
                name: s.name,
                rows: s.rows,
                cols: s.cols,
            })
            .collect(),
    }))
}

// ---------------------------------------------------------------------------
// blocks / ops / cells
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BlocksQuery {
    since: Option<String>,
}

async fn list_blocks(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
    Query(q): Query<BlocksQuery>,
) -> ApiResult<Json<Vec<Block>>> {
    if app.get_document(&doc_id)?.is_none() {
        return Err(not_found(format!("document {doc_id}")));
    }
    Ok(Json(app.list_blocks(&doc_id, q.since.as_deref())?))
}

/// openapi POST /blocks:提交一个外部签名的 block,节点校验后吸收。
async fn submit_block(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
    Json(block): Json<Block>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if block.header.doc_id != doc_id {
        return Err(ApiError(
            anyhow::anyhow!("block doc_id mismatch"),
            StatusCode::BAD_REQUEST,
        ));
    }
    if app.get_document(&doc_id)?.is_none() {
        return Err(not_found(format!("document {doc_id}")));
    }
    // 外部提交的 block 作为中继再广播一次(gossip 按消息 id 去重,无害)
    match app.ingest(block, true)? {
        AcceptOutcome::Applied | AcceptOutcome::Duplicate => Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({"status": "accepted"})),
        )),
        AcceptOutcome::Rejected(reason) => Err(ApiError(
            anyhow::anyhow!("block rejected: {reason}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// 扩展:提交一批 CellOp,由本节点签名成 block(UI 的保存通道)。
/// `stamp.ts == 0` 的 op 由服务端统一盖时间。
async fn submit_ops(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
    Json(ops): Json<Vec<CellOp>>,
) -> ApiResult<(StatusCode, Json<Block>)> {
    if ops.is_empty() {
        return Err(ApiError(
            anyhow::anyhow!("empty ops"),
            StatusCode::BAD_REQUEST,
        ));
    }
    let block = app.apply_local_ops(&doc_id, ops)?;
    Ok((StatusCode::CREATED, Json(block)))
}

fn grid_of(app: &AppState, doc_id: &str) -> ApiResult<WorkbookGrid> {
    app.document_grid(doc_id)?
        .ok_or_else(|| not_found(format!("document {doc_id}")))
}

/// 扩展:整簿计算网格。
async fn get_cells(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
) -> ApiResult<Json<WorkbookGrid>> {
    Ok(Json(grid_of(&app, &doc_id)?))
}

// ---------------------------------------------------------------------------
// 导入导出
// ---------------------------------------------------------------------------

async fn export_xlsx(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
) -> ApiResult<Response> {
    let bytes = app
        .with_workbook(&doc_id, meshsexcel_core::xlsx::workbook_to_xlsx)
        .ok_or_else(|| not_found(format!("document {doc_id}")))?
        .map_err(ApiError::from)?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{doc_id}.xlsx\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize)]
struct CsvQuery {
    sheet: Option<String>,
}

async fn export_csv(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
    Query(q): Query<CsvQuery>,
) -> ApiResult<Response> {
    let csv = app
        .with_workbook(&doc_id, |wb| {
            let sheet = q
                .sheet
                .clone()
                .unwrap_or_else(|| wb.sheet_names().first().cloned().unwrap_or_default());
            meshsexcel_core::xlsx::workbook_sheet_to_csv(wb, &sheet)
        })
        .ok_or_else(|| not_found(format!("document {doc_id}")))?
        .map_err(ApiError::from)?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{doc_id}.csv\""),
            ),
        ],
        csv.into_bytes(),
    )
        .into_response())
}

/// 扩展:导入 XLSX(请求体为文件字节),每张表一个 sheet。
async fn import_xlsx(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    if app.get_document(&doc_id)?.is_none() {
        return Err(not_found(format!("document {doc_id}")));
    }
    let sheets = meshsexcel_core::xlsx::xlsx_to_sheets(&body)
        .map_err(|e| ApiError(e.into(), StatusCode::BAD_REQUEST))?;
    let mut total = 0usize;
    for sheet in sheets {
        let mut ops = Vec::new();
        ops.push(op_add_sheet(&sheet.name));
        for cell in sheet.cells {
            ops.push(op_set_cell(
                &sheet.name,
                cell.row,
                cell.col,
                cell.value,
                cell.formula,
                None,
            ));
        }
        total += ops.len() - 1;
        commit_chunked(&app, &doc_id, ops)?;
    }
    Ok(Json(serde_json::json!({ "imported_cells": total })))
}

#[derive(Deserialize)]
struct ImportCsvQuery {
    sheet: Option<String>,
}

/// 扩展:导入 CSV(请求体为文本),进指定 sheet(默认按时间命名)。
async fn import_csv(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
    Query(q): Query<ImportCsvQuery>,
    body: String,
) -> ApiResult<Json<serde_json::Value>> {
    if app.get_document(&doc_id)?.is_none() {
        return Err(not_found(format!("document {doc_id}")));
    }
    let sheet_name = q
        .sheet
        .unwrap_or_else(|| format!("CSV-{}", chrono::Utc::now().format("%H%M%S")));
    let grid_csv = meshsexcel_core::xlsx::csv_to_grid(&body)
        .map_err(|e| ApiError(e.into(), StatusCode::BAD_REQUEST))?;
    let mut ops = vec![op_add_sheet(&sheet_name)];
    let mut total = 0usize;
    for (ri, row) in grid_csv.iter().enumerate() {
        for (ci, v) in row.iter().enumerate() {
            if v.is_empty() {
                continue;
            }
            ops.push(op_set_cell(
                &sheet_name,
                ri as u32 + 1,
                ci as u32 + 1,
                Some(v.clone()),
                None,
                None,
            ));
            total += 1;
        }
    }
    commit_chunked(&app, &doc_id, ops)?;
    Ok(Json(
        serde_json::json!({ "imported_cells": total, "sheet": sheet_name }),
    ))
}

fn op_add_sheet(name: &str) -> CellOp {
    CellOp::new(
        0,
        "",
        meshsexcel_core::ops::OpKind::AddSheet {
            sheet: name.to_string(),
            position: None,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn op_set_cell(
    sheet: &str,
    row: u32,
    col: u32,
    value: Option<String>,
    formula: Option<String>,
    style: Option<meshsexcel_core::CellStyle>,
) -> CellOp {
    CellOp::new(
        0,
        "",
        meshsexcel_core::ops::OpKind::SetCell {
            sheet: sheet.to_string(),
            row,
            col,
            value,
            formula,
            style,
        },
    )
}

/// 大导入按 400 op 一块拆 block,避免超过 gossip 消息上限。
fn commit_chunked(app: &AppState, doc_id: &str, ops: Vec<CellOp>) -> ApiResult<()> {
    for chunk in ops.chunks(400) {
        app.apply_local_ops(doc_id, chunk.to_vec())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 快照
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SnapshotCreate {
    #[serde(default)]
    description: Option<String>,
}

async fn create_snapshot(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
    Json(body): Json<SnapshotCreate>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let id = app.create_snapshot(&doc_id, body.description.as_deref())?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "snapshot_id": id })),
    ))
}

async fn list_snapshots(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
) -> ApiResult<Json<Vec<meshsexcel_core::store::SnapshotMeta>>> {
    Ok(Json(app.list_snapshots(&doc_id)?))
}

#[derive(Deserialize)]
struct RestoreReq {
    snapshot_id: String,
}

async fn restore_snapshot(
    State(app): State<Arc<AppState>>,
    Path(doc_id): Path<String>,
    Json(body): Json<RestoreReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let n = app.restore_snapshot(&doc_id, &body.snapshot_id)?;
    Ok(Json(serde_json::json!({ "restored_ops": n })))
}
