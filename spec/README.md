CREATE TABLE documents (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  owner TEXT,
  head_block TEXT,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE blocks (
  block_hash TEXT PRIMARY KEY,
  doc_id TEXT NOT NULL,
  prev_hash TEXT,
  author TEXT,
  author_pubkey TEXT,
  timestamp DATETIME,
  payload BLOB,
  merkle_root TEXT,
  signature BLOB,
  FOREIGN KEY(doc_id) REFERENCES documents(id)
);

CREATE TABLE snapshots (
  snapshot_id TEXT PRIMARY KEY,
  doc_id TEXT NOT NULL,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  snapshot_blob BLOB,
  description TEXT
);

CREATE TABLE peers (
  peer_id TEXT PRIMARY KEY,
  addr TEXT,
  pubkey TEXT,
  last_seen DATETIME
);

CREATE TABLE cells_cache (
  doc_id TEXT,
  sheet TEXT,
  row INTEGER,
  col INTEGER,
  value TEXT,
  formula TEXT,
  style TEXT,
  PRIMARY KEY(doc_id, sheet, row, col)
);
