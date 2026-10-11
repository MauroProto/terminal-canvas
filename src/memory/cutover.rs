//! Verified v2 -> v3 cutover used by app startup and the explicit CLI upgrade.
//! Existing prepared v2 writers are fenced by committed schema triggers, not by
//! the version integer alone. Failed migration rolls back the one SQLite tx.

use anyhow::{anyhow, Context, Result};
use rusqlite::{Connection, DatabaseName, OpenFlags, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct CutoverReceipt {
    pub source: PathBuf,
    pub backup: PathBuf,
    pub backup_sha256: String,
    pub logical_sha256: String,
    pub previous_version: i64,
    pub current_version: i64,
    pub migration_committed: bool,
}

const MAX_BACKUP_BYTES: i64 = 1024 * 1024 * 1024;

pub(super) fn upgrade(path: &Path, backup: &Path) -> Result<CutoverReceipt> {
    let source = path.canonicalize().context("existing memory DB required")?;
    anyhow::ensure!(!backup.exists(), "backup destination must be new");
    let parent = backup
        .parent()
        .ok_or_else(|| anyhow!("backup requires a parent"))?;
    let parent = parent.canonicalize().context("backup parent must exist")?;
    let backup = parent.join(
        backup
            .file_name()
            .ok_or_else(|| anyhow!("backup filename missing"))?,
    );
    anyhow::ensure!(backup != source, "backup cannot be the source DB");
    let mut conn = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    super::store::configure(&conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
        r.get(0)
    })?;
    anyhow::ensure!(
        version == 2,
        "explicit cutover requires schema 2, found {version}"
    );
    let pages: i64 = tx.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = tx.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    anyhow::ensure!(
        pages
            .checked_mul(page_size)
            .is_some_and(|n| n <= MAX_BACKUP_BYTES),
        "backup exceeds the explicit 1 GiB bound"
    );
    let logical_sha256 = logical_digest(&tx)?;
    // serialize uses this connection's pager snapshot, including WAL pages.
    // Unlike online backup, it does not open a competing source transaction.
    let mut bytes = tx.serialize(DatabaseName::Main)?.to_vec();
    anyhow::ensure!(
        bytes.len() >= 100 && &bytes[..16] == b"SQLite format 3\0",
        "invalid serialized database"
    );
    // The standalone snapshot has no WAL sidecars. Normalize the journal mode
    // header before reopening it read-only; all SQL contents are verified below.
    bytes[18] = 1;
    bytes[19] = 1;
    let backup_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&backup)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    let check = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    anyhow::ensure!(
        check.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))? == "ok",
        "backup integrity failed"
    );
    anyhow::ensure!(
        !check
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some(),
        "backup foreign-key validation failed"
    );
    anyhow::ensure!(
        logical_digest(&check)? == logical_sha256,
        "backup differs from locked source snapshot"
    );
    anyhow::ensure!(
        check.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))?
            == 2,
        "backup schema changed"
    );
    drop(check);
    let mut receipt = CutoverReceipt {
        source,
        backup: backup.clone(),
        backup_sha256,
        logical_sha256,
        previous_version: 2,
        current_version: 3,
        migration_committed: false,
    };
    // Persist recovery evidence before DDL. After an interrupted return, callers
    // inspect actual schema rather than interpreting this file as a commit ACK.
    let evidence = backup.with_extension("cutover.json");
    let mut evidence_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&evidence)?;
    evidence_file.write_all(&serde_json::to_vec_pretty(&receipt)?)?;
    evidence_file.sync_all()?;
    if let Ok(dir) = std::fs::File::open(&parent) {
        dir.sync_all()?;
    }
    super::store::migrate_locked(&tx, true)?;
    tx.commit()?;
    receipt.migration_committed = true;
    Ok(receipt)
}

/// Preserving generations takes precedence over old-binary compatibility.
/// A v3 DB is never overwritten with its old backup: even retired generations
/// remain tombstones. Old binaries may inspect the verified v2 backup separately.
pub fn explain_downgrade(path: &Path) -> Result<String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let count: i64 =
        conn.query_row("SELECT COUNT(*) FROM tc_hook_generations", [], |r| r.get(0))?;
    Ok(format!("Schema 3 retained with {count} generation records. In-place downgrade is refused. Keep this DB and cutover receipt; inspect the v2 backup under a separate path only. A running managed endpoint must continue using schema 3.\n"))
}

pub(super) fn logical_digest(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare("SELECT name,sql FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
    let tables = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut hash = Sha256::new();
    for (name, sql) in tables {
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(sql.unwrap_or_default().as_bytes());
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))?;
        let columns = stmt.column_count();
        let rows = stmt.query_map([], |r| {
            let mut values = Vec::new();
            for column in 0..columns {
                let value = r.get_ref(column)?;
                let encoded = match value {
                    rusqlite::types::ValueRef::Null => vec![0],
                    rusqlite::types::ValueRef::Integer(n) => {
                        [vec![1], n.to_le_bytes().to_vec()].concat()
                    }
                    rusqlite::types::ValueRef::Real(n) => {
                        [vec![2], n.to_bits().to_le_bytes().to_vec()].concat()
                    }
                    rusqlite::types::ValueRef::Text(b) => [vec![3], b.to_vec()].concat(),
                    rusqlite::types::ValueRef::Blob(b) => [vec![4], b.to_vec()].concat(),
                };
                values.extend_from_slice(&(encoded.len() as u64).to_le_bytes());
                values.extend_from_slice(&encoded);
            }
            Ok(values)
        })?;
        let mut rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.sort();
        for row in rows {
            hash.update((row.len() as u64).to_le_bytes());
            hash.update(row);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}
