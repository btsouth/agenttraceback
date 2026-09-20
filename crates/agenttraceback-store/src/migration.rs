use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, TransactionBehavior, params};

use crate::{StoreError, StoreResult};

pub(crate) const TARGET_SCHEMA_VERSION: i64 = 4;
const MIGRATIONS: &[(i64, &str, &str)] = &[
    (
        1,
        "0001_initial",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../migrations/0001_initial.sql"
        )),
    ),
    (
        2,
        "0002_recovery_plans",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../migrations/0002_recovery_plans.sql"
        )),
    ),
    (
        3,
        "0003_adapter_hooks",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../migrations/0003_adapter_hooks.sql"
        )),
    ),
    (
        4,
        "0004_audited_deletion",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../migrations/0004_audited_deletion.sql"
        )),
    ),
];

pub(crate) fn open_writer(path: &Path, backup_root: &Path) -> StoreResult<Connection> {
    let mut connection = Connection::open(path).map_err(StoreError::Sqlite)?;
    set_private_file(path)?;
    configure_common(&connection)?;
    let current_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(StoreError::Sqlite)?;

    if current_version > TARGET_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema {
            found: current_version,
            supported: TARGET_SCHEMA_VERSION,
        });
    }
    if current_version > 0 && current_version < TARGET_SCHEMA_VERSION {
        create_pre_migration_backup(&connection, backup_root, current_version)?;
    }
    if current_version < TARGET_SCHEMA_VERSION {
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(StoreError::Sqlite)?;
        for (version, name, sql) in MIGRATIONS
            .iter()
            .filter(|(version, _, _)| *version > current_version)
        {
            apply_migration(&mut connection, *version, name, sql)?;
        }
        connection
            .pragma_update(None, "synchronous", "NORMAL")
            .map_err(StoreError::Sqlite)?;
    }
    Ok(connection)
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> StoreResult<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> StoreResult<()> {
    Ok(())
}

pub(crate) fn open_reader(path: &Path) -> StoreResult<Connection> {
    let connection = Connection::open(path).map_err(StoreError::Sqlite)?;
    configure_common(&connection)?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(StoreError::Sqlite)?;
    Ok(connection)
}

fn configure_common(connection: &Connection) -> StoreResult<()> {
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(StoreError::Sqlite)?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(StoreError::Sqlite)?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(StoreError::Sqlite)?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(StoreError::Sqlite)?;
    connection
        .pragma_update(None, "wal_autocheckpoint", 1_000)
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn apply_migration(
    connection: &mut Connection,
    version: i64,
    name: &str,
    sql: &str,
) -> StoreResult<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Sqlite)?;
    transaction.execute_batch(sql).map_err(StoreError::Sqlite)?;
    transaction
        .execute(
            "INSERT INTO schema_migrations(version, name, applied_at_us) VALUES (?1, ?2, ?3)",
            params![version, name, current_time_us()],
        )
        .map_err(StoreError::Sqlite)?;
    transaction
        .pragma_update(None, "user_version", version)
        .map_err(StoreError::Sqlite)?;
    transaction.commit().map_err(StoreError::Sqlite)
}

fn create_pre_migration_backup(
    connection: &Connection,
    backup_root: &Path,
    current_version: i64,
) -> StoreResult<()> {
    fs::create_dir_all(backup_root).map_err(|source| StoreError::Io {
        path: backup_root.to_path_buf(),
        source,
    })?;
    let backup_path = unique_backup_path(backup_root, current_version);
    let backup_path_text = backup_path.to_string_lossy();
    connection
        .execute("VACUUM INTO ?1", [backup_path_text.as_ref()])
        .map_err(StoreError::Sqlite)?;
    Ok(())
}

fn unique_backup_path(backup_root: &Path, schema_version: i64) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    backup_root.join(format!(
        "agenttraceback-pre-migration-v{schema_version}-{timestamp}.db"
    ))
}

pub(crate) fn current_time_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}
