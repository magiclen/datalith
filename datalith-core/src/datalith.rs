use std::{
    collections::HashSet,
    fmt::{self, Debug, Formatter},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Duration,
};

use chrono::prelude::*;
use sqlx::{
    Pool, Sqlite,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use tokio::{fs, sync::Notify};
pub use uuid::Uuid;

use crate::{
    DatalithCreateError, DatalithReadError,
    functions::allow_not_found_error,
    guard::{DeleteGuard, FileLifecycle},
};

/// The name of the SQLite database file.
pub const PATH_DB_FILE: &str = "datalith.sqlite";
/// The directory name for temporary upload files.
pub const PATH_TEMPORARY_FILE_DIRECTORY: &str = "datalith.temp";
/// The directory name for stored file contents.
pub const PATH_FILE_DIRECTORY: &str = "datalith.files";

const DATABASE_VERSION: u32 = 2;
const MAX_DATABASE_CONNECTIONS: u32 = 4;

#[derive(Debug)]
pub(crate) struct DatalithInner {
    pub(crate) db:              Pool<Sqlite>,
    pub(crate) _service_active: AtomicBool,
    environment:                PathBuf,
    _create_time:               DateTime<Local>,
    _version:                   u32,
    pub(crate) _file_lifecycle: Mutex<FileLifecycle>,
    pub(crate) _file_changed:   Notify,
    _sql_file:                  std::fs::File,
}

/// A Datalith file store.
#[derive(Clone)]
pub struct Datalith(pub(crate) Arc<DatalithInner>);

impl Debug for Datalith {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Debug::fmt(self.0.as_ref(), f)
    }
}

impl Datalith {
    /// Get the root directory of this store.
    #[inline]
    pub fn get_environment(&self) -> &Path {
        self.0.environment.as_path()
    }
}

impl Datalith {
    async fn get_directory(&self, path: impl AsRef<str>) -> io::Result<PathBuf> {
        let directory = self.0.environment.join(path.as_ref());

        match fs::metadata(directory.as_path()).await {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{directory:?} is not a directory"),
                    ));
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(directory.as_path()).await?;
            },
            Err(error) => return Err(error),
        }

        Ok(directory)
    }

    #[inline]
    async fn get_file_directory(&self) -> io::Result<PathBuf> {
        self.get_directory(PATH_FILE_DIRECTORY).await
    }

    #[inline]
    pub(crate) async fn get_file_path(&self, id: Uuid) -> io::Result<PathBuf> {
        let storage_id: Option<(Uuid,)> =
            sqlx::query_as("SELECT storage_id FROM blob_files WHERE file_id = ?")
                .bind(id)
                .fetch_optional(&self.0.db)
                .await
                .map_err(io::Error::other)?;
        let storage_id = storage_id.map_or(id, |(id,)| id);
        Ok(self.storage_path(storage_id))
    }

    // The file directory is created before any file is stored, so reads and removals do not need to check it.
    fn storage_path(&self, storage_id: Uuid) -> PathBuf {
        self.0.environment.join(PATH_FILE_DIRECTORY).join(format!("{:x}", storage_id.as_u128()))
    }

    #[inline]
    async fn get_temporary_directory(&self) -> io::Result<PathBuf> {
        self.get_directory(PATH_TEMPORARY_FILE_DIRECTORY).await
    }
}

// Open and close
impl Datalith {
    /// Open a Datalith store, or create one if it does not exist.
    pub async fn new(environment_path: impl AsRef<Path>) -> Result<Self, DatalithCreateError> {
        let environment_path_ref = environment_path.as_ref();

        let environment_path = match fs::canonicalize(environment_path_ref).await {
            Ok(environment_path_canonical) => {
                if !environment_path_canonical.is_dir() {
                    return Err(DatalithCreateError::IOError(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{environment_path_canonical:?} exists but it is not a directory"),
                    )));
                }

                environment_path_canonical
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(environment_path_ref).await?;

                fs::canonicalize(environment_path_ref).await?
            },
            Err(error) => return Err(error.into()),
        };

        let sql_file_path = environment_path.join(PATH_DB_FILE);

        let sql_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&sql_file_path)?;
        match sql_file.try_lock() {
            Ok(()) => (),
            Err(std::fs::TryLockError::WouldBlock) => return Err(DatalithCreateError::AlreadyRun),
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        let sql_options = SqliteConnectOptions::new()
            .filename(&sql_file_path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(30));
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(MAX_DATABASE_CONNECTIONS)
            .connect_with(sql_options)
            .await?;
        let (version, create_time) =
            match Self::initial_with_migration(&pool, &environment_path).await {
                Ok(information) => information,
                Err(error) => {
                    pool.close().await;
                    return Err(error);
                },
            };

        let datalith = Self(Arc::new(DatalithInner {
            db:              pool,
            _service_active: AtomicBool::new(false),
            environment:     environment_path,
            _create_time:    create_time,
            _version:        version,
            _file_lifecycle: Mutex::new(FileLifecycle::default()),
            _file_changed:   Notify::new(),
            _sql_file:       sql_file,
        }));

        // Remove temporary files left by earlier uploads.
        {
            let temporary_directory = datalith.get_temporary_directory().await?;

            let mut read_dir = fs::read_dir(temporary_directory.as_path()).await?;

            if read_dir.next_entry().await?.is_some() {
                fs::remove_dir_all(temporary_directory.as_path()).await?;

                datalith.get_temporary_directory().await?;
            }
        }

        Ok(datalith)
    }

    async fn initial_with_migration(
        pool: &Pool<Sqlite>,
        environment: &Path,
    ) -> Result<(u32, DateTime<Local>), DatalithCreateError> {
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sys_db_information'",
        )
        .fetch_one(pool)
        .await?;
        if exists == 0 {
            let mut tx = pool.begin().await?;
            sqlx::query(
                "CREATE TABLE sys_db_information(key TEXT PRIMARY KEY NOT NULL, value TEXT)",
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO sys_db_information(key,value) VALUES('version',?),('create_time',?)",
            )
            .bind(DATABASE_VERSION.to_string())
            .bind(Local::now().to_rfc3339())
            .execute(&mut *tx)
            .await?;
            sqlx::raw_sql(include_str!("sql/schema.sql")).execute(&mut *tx).await?;
            sqlx::raw_sql(include_str!("sql/service.sql")).execute(&mut *tx).await?;
            tx.commit().await?;
        }
        let version: String =
            sqlx::query_scalar("SELECT value FROM sys_db_information WHERE key='version'")
                .fetch_one(pool)
                .await?;
        let version: u32 =
            version.parse().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if version > DATABASE_VERSION {
            return Err(DatalithCreateError::DatabaseTooNewError {
                app_db_version:     DATABASE_VERSION,
                current_db_version: version,
            });
        }
        if version < 1 {
            return Err(DatalithCreateError::DatabaseTooOldError {
                app_db_version:     DATABASE_VERSION,
                current_db_version: version,
            });
        }
        if version == 1 {
            crate::service::migration::upgrade(pool, environment).await?;
        }
        // Version 2 stores created before the crash counter existed do not have its column yet.
        let crash_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name = 'crash_count'",
        )
        .fetch_one(pool)
        .await?;
        if crash_count == 0 {
            sqlx::query("ALTER TABLE tasks ADD COLUMN crash_count INTEGER NOT NULL DEFAULT 0")
                .execute(pool)
                .await?;
        }
        let created: String =
            sqlx::query_scalar("SELECT value FROM sys_db_information WHERE key='create_time'")
                .fetch_one(pool)
                .await?;
        let created = DateTime::parse_from_rfc3339(&created)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok((DATABASE_VERSION, created.into()))
    }

    /// Close the Datalith database.
    #[inline]
    pub async fn close(self) {
        self.0.db.close().await;
    }

    /// Close the database and remove all files owned by this Datalith store.
    #[inline]
    pub async fn drop_datalith(self) -> Result<(), io::Error> {
        self.0.db.close().await;

        // Remove only files owned by this store.
        let mut names = vec![
            PATH_DB_FILE.to_owned(),
            format!("{PATH_DB_FILE}-wal"),
            format!("{PATH_DB_FILE}-shm"),
        ];
        // Upgrades keep one backup for each older database version.
        for version in 1..DATABASE_VERSION {
            names.push(format!("{PATH_DB_FILE}.v{version}.bak"));
            names.push(format!("{PATH_DB_FILE}.v{version}.bak.pending"));
        }
        for name in names {
            allow_not_found_error(fs::remove_file(self.0.environment.join(name)).await)?;
        }
        allow_not_found_error(fs::remove_dir_all(self.0.environment.join("datalith.tasks")).await)?;
        allow_not_found_error(
            fs::remove_dir_all(self.0.environment.join(PATH_TEMPORARY_FILE_DIRECTORY)).await,
        )?;
        allow_not_found_error(
            fs::remove_dir_all(self.0.environment.join(PATH_FILE_DIRECTORY)).await,
        )?;

        match fs::read_dir(self.0.environment.as_path()).await {
            Ok(mut dir) => {
                if dir.next_entry().await?.is_none() {
                    // Remove the environment directory if it is empty.
                    allow_not_found_error(fs::remove_dir(self.0.environment.as_path()).await)?;
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }

        Ok(())
    }
}

impl Datalith {
    /// Clear untracked files in the file system.
    pub(crate) async fn clear_untracked_files(&self) -> Result<usize, DatalithReadError> {
        let file_directory = self.get_file_directory().await?;
        // Load every storage ID in use with one query; only the other entries need the careful checks below, which run again before removing anything.
        let used_storage_ids: HashSet<Uuid> = sqlx::query_scalar(
            "SELECT COALESCE(b.storage_id, f.id) FROM files f LEFT JOIN blob_files b ON b.file_id \
             = f.id",
        )
        .fetch_all(&self.0.db)
        .await?
        .into_iter()
        .collect();
        let mut directory = fs::read_dir(file_directory).await?;
        let mut counter = 0;
        while let Some(entry) = directory.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                allow_not_found_error(fs::remove_dir_all(path).await)?;
                counter += 1;
                continue;
            }
            let id = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| u128::from_str_radix(name, 16).ok())
                .map(Uuid::from_u128);
            if let Some(id) = id {
                if used_storage_ids.contains(&id) {
                    continue;
                }
                let _guard = DeleteGuard::new(self.clone(), id).await;
                let aliases: Vec<Uuid> =
                    sqlx::query_scalar("SELECT file_id FROM blob_files WHERE storage_id = ?")
                        .bind(id)
                        .fetch_all(&self.0.db)
                        .await?;
                {
                    let lifecycle = self.0._file_lifecycle.lock().unwrap();
                    if lifecycle.opening.contains_key(&id)
                        || aliases.iter().any(|id| lifecycle.opening.contains_key(id))
                    {
                        continue;
                    }
                }
                let tracked = sqlx::query(
                    "SELECT 1 FROM files LEFT JOIN blob_files ON files.id = blob_files.file_id
                     WHERE files.id = ? AND (blob_files.storage_id IS NULL OR \
                     blob_files.storage_id = files.id)
                     UNION ALL SELECT 1 FROM files JOIN blob_files ON files.id = \
                     blob_files.file_id WHERE storage_id = ? LIMIT 1",
                )
                .bind(id)
                .bind(id)
                .fetch_optional(&self.0.db)
                .await?
                .is_some();
                if tracked {
                    continue;
                }
                allow_not_found_error(fs::remove_file(path).await)?;
            } else {
                allow_not_found_error(fs::remove_file(path).await)?;
            }
            counter += 1;
        }
        let untracked: Vec<Uuid> = sqlx::query_scalar(
            "SELECT file_id FROM blob_files WHERE NOT EXISTS (SELECT 1 FROM files WHERE id = \
             file_id)",
        )
        .fetch_all(&self.0.db)
        .await?;
        for id in untracked {
            let _guard = DeleteGuard::new(self.clone(), id).await;
            if self.0._file_lifecycle.lock().unwrap().opening.contains_key(&id) {
                continue;
            }
            sqlx::query(
                "DELETE FROM blob_files WHERE file_id = ? AND NOT EXISTS (SELECT 1 FROM files \
                 WHERE id = ?)",
            )
            .bind(id)
            .bind(id)
            .execute(&self.0.db)
            .await?;
        }
        Ok(counter)
    }

    pub(crate) async fn clear_untracked_storage_file(
        &self,
        storage_id: Uuid,
    ) -> Result<bool, DatalithReadError> {
        let mut ids: HashSet<Uuid> =
            sqlx::query_scalar("SELECT file_id FROM blob_files WHERE storage_id = ?")
                .bind(storage_id)
                .fetch_all(&self.0.db)
                .await?
                .into_iter()
                .collect();
        ids.insert(storage_id);
        let Some(_guards) = DeleteGuard::try_acquire_multiple(self.clone(), &ids) else {
            return Ok(false);
        };
        let mut tx = self.0.db.begin_with("BEGIN IMMEDIATE").await?;
        let tracked = sqlx::query(
            "SELECT 1 FROM files f LEFT JOIN blob_files b ON b.file_id = f.id WHERE f.id = ? AND \
             (b.storage_id IS NULL OR b.storage_id = f.id) UNION ALL SELECT 1 FROM blob_files b \
             JOIN files f ON f.id = b.file_id WHERE b.storage_id = ? LIMIT 1",
        )
        .bind(storage_id)
        .bind(storage_id)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        if !tracked {
            allow_not_found_error(fs::remove_file(self.storage_path(storage_id)).await)?;
        }
        sqlx::query(
            "DELETE FROM blob_files WHERE storage_id = ? AND NOT EXISTS (SELECT 1 FROM files \
             WHERE id = file_id)",
        )
        .bind(storage_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}

impl Datalith {
    pub(crate) async fn release_file_references_in_transaction(
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        id: Uuid,
        count: u64,
    ) -> Result<bool, sqlx::Error> {
        let changed = sqlx::query("UPDATE files SET count = count - ? WHERE id = ? AND count > ?")
            .bind(count as i64)
            .bind(id)
            .bind(count as i64)
            .execute(&mut **tx)
            .await?;
        if changed.rows_affected() > 0 {
            return Ok(false);
        }
        let removed = sqlx::query("DELETE FROM files WHERE id = ? AND count = ?")
            .bind(id)
            .bind(count as i64)
            .execute(&mut **tx)
            .await?;
        Ok(removed.rows_affected() > 0)
    }
}
