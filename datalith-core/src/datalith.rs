#[cfg(feature = "image-convert")]
use std::sync::atomic::{AtomicU8, AtomicU32};
use std::{
    collections::HashSet,
    fmt::{self, Debug, Formatter},
    future::Future,
    io,
    io::ErrorKind,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::prelude::*;
use educe::Educe;
use mime::Mime;
use rdb_pagination::{Pagination, PaginationOptions, SqlJoin, SqlOrderByComponent, prelude::*};
use sha2::{Digest, Sha256};
use sqlx::{
    Pool, Sqlite,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use tokio::{
    fs,
    fs::File,
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::{Notify, Semaphore},
    task::JoinSet,
    time,
};
pub use uuid::Uuid;

use crate::{
    DEFAULT_MIME_TYPE, DatalithCreateError, DatalithFile, DatalithReadError, DatalithWriteError,
    functions::{
        BUFFER_SIZE, allow_not_found_error, calculate_buffer_size, detect_file_type_by_buffer,
        detect_file_type_by_path, get_current_timestamp, get_file_name, get_hash_by_buffer,
        get_random_hash,
    },
    guard::{DeleteGuard, FileLifecycle, OpenGuard, PutGuard, TemporaryFileGuard},
};

/// The name of the SQLite database file.
pub const PATH_DB_FILE: &str = "datalith.sqlite";
/// The directory name for temporary upload files.
pub const PATH_TEMPORARY_FILE_DIRECTORY: &str = "datalith.temp";
/// The directory name for stored file contents.
pub const PATH_FILE_DIRECTORY: &str = "datalith.files";

const DATABASE_VERSION: u32 = 2;
const MAX_DATABASE_CONNECTIONS: u32 = 4;

const TEMPORARY_FILE_LIFESPAN: Duration = Duration::from_secs(60);

#[cfg(feature = "image-convert")]
const MAX_IMAGE_RESOLUTION: u32 = 50_000_000; // 50MP
#[cfg(feature = "image-convert")]
const MAX_IMAGE_RESOLUTION_MULTIPLIER: u8 = 3; // 1x, 2x, 3x

/// Sort options for file queries.
#[derive(Debug, Clone, Educe, OrderByOptions)]
#[educe(Default)]
#[orderByOptions(name = files)]
pub struct DatalithFileOrderBy {
    #[educe(Default = 102)]
    #[orderByOptions((files, id), unique)]
    pub id:         OrderMethod,
    #[educe(Default = -101)]
    #[orderByOptions((files, created_at))]
    pub created_at: OrderMethod,
    #[orderByOptions((files, expired_at))]
    pub expired_at: OrderMethod,
    #[orderByOptions((files, file_size))]
    pub file_size:  OrderMethod,
    #[orderByOptions((files, file_type))]
    pub file_type:  OrderMethod,
    #[orderByOptions((files, file_name))]
    pub file_name:  OrderMethod,
}

#[derive(Educe)]
#[educe(Debug(name(Datalith)))]
pub(crate) struct DatalithInner {
    pub(crate) db:                               Pool<Sqlite>,
    pub(crate) _service_active:                  AtomicBool,
    environment:                                 PathBuf,
    _create_time:                                DateTime<Local>,
    _version:                                    u32,
    pub(crate) _uploading_files:                 Mutex<HashSet<[u8; 32]>>,
    pub(crate) _file_lifecycle:                  Mutex<FileLifecycle>,
    pub(crate) _file_changed:                    Notify,
    _sql_file:                                   std::fs::File,
    pub(crate) _temporary_file_lifespan:         AtomicU64,
    #[cfg(feature = "image-convert")]
    pub(crate) _max_image_resolution:            AtomicU32,
    #[cfg(feature = "image-convert")]
    pub(crate) _max_image_resolution_multiplier: AtomicU8,
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

    /// Get the lifetime of temporary uploads.
    #[inline]
    pub fn get_temporary_file_lifespan(&self) -> Duration {
        let milli_secs = self.0._temporary_file_lifespan.load(Ordering::Relaxed);

        Duration::from_millis(milli_secs)
    }

    /// Set the lifetime of temporary uploads.
    ///
    /// The allowed lifetime is **100 milliseconds** to **10000 hours**.
    #[inline]
    pub fn set_temporary_file_lifespan(&self, mut temporary_file_lifespan: Duration) {
        const ONE_TENTH_SECOND: Duration = Duration::from_millis(100);
        const TEN_THOUSANDS_HOUR: Duration = Duration::from_secs(10000 * 60 * 60);

        if temporary_file_lifespan < ONE_TENTH_SECOND {
            temporary_file_lifespan = ONE_TENTH_SECOND
        } else if temporary_file_lifespan > TEN_THOUSANDS_HOUR {
            temporary_file_lifespan = TEN_THOUSANDS_HOUR;
        }

        self.0
            ._temporary_file_lifespan
            .swap(temporary_file_lifespan.as_millis() as u64, Ordering::Relaxed);
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
        Ok(self.get_file_directory().await?.join(format!("{:x}", storage_id.as_u128())))
    }

    #[inline]
    async fn get_temporary_directory(&self) -> io::Result<PathBuf> {
        self.get_directory(PATH_TEMPORARY_FILE_DIRECTORY).await
    }

    #[inline]
    pub(crate) async fn get_temporary_file_path(&self, temporary_id: Uuid) -> io::Result<PathBuf> {
        Ok(self.get_temporary_directory().await?.join(format!("{:x}", temporary_id.as_u128())))
    }

    #[inline]
    pub(crate) fn get_expired_timestamp<Tz: TimeZone>(&self, current_time: DateTime<Tz>) -> i64 {
        current_time.timestamp_millis() + self.get_temporary_file_lifespan().as_millis() as i64
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

                // The directory was just created, so this path should exist.
                fs::canonicalize(environment_path_ref).await.unwrap()
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
        let (version, create_time) = Self::initial_with_migration(&pool, &environment_path).await?;

        let uploading_files = Mutex::new(HashSet::new());
        let file_lifecycle = Mutex::new(FileLifecycle::default());

        let datalith = Self(Arc::new(DatalithInner {
            db:                                                                 pool,
            _service_active:                                                    AtomicBool::new(
                false,
            ),
            environment:                                                        environment_path,
            _create_time:                                                       create_time,
            _version:                                                           version,
            _uploading_files:                                                   uploading_files,
            _file_lifecycle:                                                    file_lifecycle,
            _file_changed:                                                      Notify::new(),
            _sql_file:                                                          sql_file,
            _temporary_file_lifespan:                                           AtomicU64::new(
                TEMPORARY_FILE_LIFESPAN.as_millis() as u64,
            ),
            #[cfg(feature = "image-convert")]
            _max_image_resolution:                                              AtomicU32::new(
                MAX_IMAGE_RESOLUTION,
            ),
            #[cfg(feature = "image-convert")]
            _max_image_resolution_multiplier:                                   AtomicU8::new(
                MAX_IMAGE_RESOLUTION_MULTIPLIER,
            ),
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
        crate::service::migration::upgrade(pool, environment).await?;
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
        for name in [
            PATH_DB_FILE.to_owned(),
            format!("{PATH_DB_FILE}-wal"),
            format!("{PATH_DB_FILE}-shm"),
            format!("{PATH_DB_FILE}.v1.bak"),
            format!("{PATH_DB_FILE}.v1.bak.pending"),
        ] {
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

/// Choose how to check a file MIME type during upload.
#[derive(Debug, Clone, Copy)]
pub enum FileTypeLevel {
    /// Require the detected MIME type to match the given type.
    ExactMatch,
    /// Use the given MIME type without detecting it.
    Manual,
    /// Use the detected MIME type, or the given type if detection fails.
    Fallback,
}

// Permanent uploads
impl Datalith {
    /// Store a file from a buffer.
    pub async fn put_file_by_buffer(
        &self,
        buffer: impl AsRef<[u8]>,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let file_data = buffer.as_ref();
        let hash = get_hash_by_buffer(file_data);

        let _put_guard = PutGuard::new(self.clone(), hash).await;

        if let Some(file) = self.get_file_by_hash(&hash).await? {
            #[rustfmt::skip]
            let result = sqlx::query(
                "
                    UPDATE
                        `files`
                    SET
                        `count` = `count` + 1
                    WHERE
                        `id` = ?
                ",
            )
            .bind(file.id())
            .execute(&self.0.db)
            .await?;

            debug_assert!(result.rows_affected() > 0);

            Ok(file)
        } else {
            self.put_file_by_buffer_inner(hash, file_data, file_name, file_type, false).await
        }
    }

    async fn put_file_by_buffer_inner(
        &self,
        hash: [u8; 32],
        file_data: &[u8],
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
        temporary: bool,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let file_type =
            handle_file_type(file_type, async { detect_file_type_by_buffer(file_data).await })
                .await?;
        let temporary_file_path = self.get_temporary_file_path(Uuid::new_v4()).await?;
        let mut file_guard = TemporaryFileGuard::new(&temporary_file_path);
        let mut file = File::create(&temporary_file_path).await?;
        file.write_all(file_data).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        self.put_file_by_reader_inner(
            hash,
            temporary_file_path,
            &mut file_guard,
            file_data.len() as u64,
            file_name,
            Some((file_type, FileTypeLevel::Manual)),
            temporary,
        )
        .await
    }

    /// Store a file from a file path.
    pub async fn put_file_by_path(
        &self,
        file_path: impl AsRef<Path>,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithFile, DatalithWriteError> {
        self.put_file_by_path_inner(file_path.as_ref(), file_name, file_type, false).await
    }

    async fn put_file_by_path_inner(
        &self,
        file_path: &Path,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
        temporary: bool,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let reader = File::open(file_path).await?;
        let temporary_file_path = self.get_temporary_file_path(Uuid::new_v4()).await?;
        let mut file_guard = TemporaryFileGuard::new(&temporary_file_path);
        let (file_size, hash) = if temporary {
            let file_size =
                get_file_size_by_reader_and_copy_to_file(reader, &temporary_file_path, None)
                    .await?;

            (file_size, get_random_hash())
        } else {
            get_file_size_and_hash_by_reader_and_copy_to_file(reader, &temporary_file_path, None)
                .await?
        };
        let _put_guard = PutGuard::new(self.clone(), hash).await;
        if !temporary && let Some(file) = self.get_file_by_hash(&hash).await? {
            sqlx::query("UPDATE files SET count = count + 1 WHERE id = ?")
                .bind(file.id())
                .execute(&self.0.db)
                .await?;
            return Ok(file);
        }
        let file_type = handle_file_type(file_type, async {
            detect_file_type_by_path(&temporary_file_path, false).await.or_else(|| {
                file_path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| mime_guess::from_ext(extension).first_or_octet_stream())
            })
        })
        .await?;
        let file_name = file_name
            .map(Into::into)
            .or_else(|| file_path.file_name().map(|name| name.to_string_lossy().into_owned()));
        self.put_file_by_reader_inner(
            hash,
            temporary_file_path,
            &mut file_guard,
            file_size,
            file_name,
            Some((file_type, FileTypeLevel::Manual)),
            temporary,
        )
        .await
    }

    /// Store a file from a reader.
    pub async fn put_file_by_reader(
        &self,
        reader: impl AsyncRead + Unpin,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
        expected_reader_length: Option<u64>,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let temporary_file_path = self.get_temporary_file_path(Uuid::new_v4()).await?;

        let mut file_guard = TemporaryFileGuard::new(temporary_file_path.as_path());

        let (file_size, hash) = get_file_size_and_hash_by_reader_and_copy_to_file(
            reader,
            temporary_file_path.as_path(),
            expected_reader_length,
        )
        .await?;

        let _put_guard = PutGuard::new(self.clone(), hash).await;

        if let Some(file) = self.get_file_by_hash(&hash).await? {
            #[rustfmt::skip]
            let result = sqlx::query(
                "
                    UPDATE
                        `files`
                    SET
                        `count` = `count` + 1
                    WHERE
                        `id` = ?
                ",
            )
            .bind(file.id())
            .execute(&self.0.db)
            .await?;

            debug_assert!(result.rows_affected() > 0);

            Ok(file)
        } else {
            self.put_file_by_reader_inner(
                hash,
                temporary_file_path,
                &mut file_guard,
                file_size,
                file_name,
                file_type,
                false,
            )
            .await
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn put_file_by_reader_inner(
        &self,
        hash: [u8; 32],
        temporary_file_path: PathBuf,
        file_guard: &mut TemporaryFileGuard,
        file_size: u64,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
        temporary: bool,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let id = Uuid::new_v4(); // we can assume this id cannot be deleted
        let created_at = Local::now();
        let file_type = handle_file_type(file_type, async {
            detect_file_type_by_path(temporary_file_path.as_path(), false).await
        })
        .await?;
        let file_name = get_file_name(file_name, created_at, &file_type);
        let expired_at =
            if temporary { Some(self.get_expired_timestamp(created_at)) } else { None };

        let mut tx = self.0.db.begin().await?;

        #[rustfmt::skip]
        let result = sqlx::query(
            "
                INSERT INTO `files` (`id`, `hash`, `created_at`, `file_size`, `file_type`, `file_name`, `expired_at`)
                    VALUES (?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(id)
        .bind(hash.to_vec())
        .bind(created_at.timestamp_millis())
        .bind(file_size as i64)
        .bind(file_type.essence_str())
        .bind(file_name.as_str())
        .bind(expired_at)
        .execute(&mut *tx)
        .await?;

        debug_assert!(result.rows_affected() > 0);

        let original_file_path = temporary_file_path;
        // A new file has no alias, so do not request another connection while holding the write transaction.
        let file_path = self.get_file_directory().await?.join(format!("{:x}", id.as_u128()));

        // Protect this ID before saving it in the database.
        let open_guard = OpenGuard::new(self.clone(), id).await;

        fs::rename(original_file_path, &file_path).await?;
        file_guard.set_moved();
        #[cfg(unix)]
        File::open(file_path.parent().unwrap()).await?.sync_all().await?;

        // The commit keeps the file guard even if the caller cancels this future.
        let open_guard = tokio::spawn(async move {
            tx.commit().await?;
            Ok::<_, sqlx::Error>(open_guard)
        })
        .await
        .map_err(io::Error::other)??;

        let file = DatalithFile::new(
            self.clone(),
            open_guard,
            id,
            created_at,
            file_size,
            file_type,
            file_name,
            expired_at.is_some(),
            true,
        );

        Ok(file)
    }
}

// Temporary uploads
impl Datalith {
    /// Store a single-use file from a buffer.
    ///
    /// A temporary file can be claimed only once through `get_file_by_id`.
    pub async fn put_file_by_buffer_temporarily(
        &self,
        buffer: impl AsRef<[u8]>,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let hash = get_random_hash(); // we can assume this hash will not be duplicated

        self.put_file_by_buffer_inner(hash, buffer.as_ref(), file_name, file_type, true).await
    }

    /// Store a single-use file from a file path.
    ///
    /// A temporary file can be claimed only once through `get_file_by_id`.
    pub async fn put_file_by_path_temporarily(
        &self,
        file_path: impl AsRef<Path>,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithFile, DatalithWriteError> {
        self.put_file_by_path_inner(file_path.as_ref(), file_name, file_type, true).await
    }

    /// Store a single-use file from a reader.
    ///
    /// A temporary file can be claimed only once through `get_file_by_id`.
    pub async fn put_file_by_reader_temporarily(
        &self,
        reader: impl AsyncRead + Unpin,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
        expected_reader_length: Option<u64>,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let temporary_file_path = self.get_temporary_file_path(Uuid::new_v4()).await?;

        let hash = get_random_hash(); // we can assume this hash will not be duplicated

        let mut file_guard = TemporaryFileGuard::new(temporary_file_path.as_path());

        let file_size = get_file_size_by_reader_and_copy_to_file(
            reader,
            temporary_file_path.as_path(),
            expected_reader_length,
        )
        .await?;

        self.put_file_by_reader_inner(
            hash,
            temporary_file_path,
            &mut file_guard,
            file_size,
            file_name,
            file_type,
            true,
        )
        .await
    }
}

// Cleanup
impl Datalith {
    /// Clear expired files and resources.
    pub async fn clear_expired_files(&self, timeout: Duration) -> Result<usize, DatalithReadError> {
        let current_timestamp = get_current_timestamp();

        #[rustfmt::skip]
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "
                SELECT
                    `id`
                FROM
                    `files`
                WHERE
                    `expired_at` <= ?
            ",
        )
        .bind(current_timestamp)
        .fetch_all(&self.0.db)
        .await?;

        let resources: Vec<(Uuid,)> =
            sqlx::query_as("SELECT id FROM resources WHERE expired_at <= ?")
                .bind(current_timestamp)
                .fetch_all(&self.0.db)
                .await?;
        // Run at most as many deletions as the database pool has connections.
        let permits = Arc::new(Semaphore::new(MAX_DATABASE_CONNECTIONS as usize));
        let mut tasks = JoinSet::new();
        for (id,) in resources {
            let permit = permits.clone().acquire_owned().await.map_err(io::Error::other)?;
            let datalith = self.clone();
            tasks.spawn(async move {
                let _permit = permit;

                time::timeout(timeout, datalith.delete_resource_by_id(id))
                    .await
                    .unwrap_or_else(|_| Ok(false))
            });
        }
        for (id,) in rows {
            let permit = permits.clone().acquire_owned().await.map_err(io::Error::other)?;
            let datalith = self.clone();
            tasks.spawn(async move {
                let _permit = permit;

                time::timeout(timeout, datalith.delete_file_by_id(id))
                    .await
                    .unwrap_or_else(|_| Ok(false))
            });
        }

        let mut counter = 0usize;

        while let Some(result) = tasks.join_next().await {
            if result.map_err(io::Error::other)?? {
                counter += 1;
            }
        }

        Ok(counter)
    }

    /// Clear untracked files in the file system.
    pub async fn clear_untracked_files(&self) -> Result<usize, DatalithReadError> {
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
            let path = self.get_file_directory().await?.join(format!("{:x}", storage_id.as_u128()));
            allow_not_found_error(fs::remove_file(path).await)?;
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

// Download
impl Datalith {
    /// Check whether the file exists.
    pub async fn check_file_exist(&self, id: impl Into<Uuid>) -> Result<bool, DatalithReadError> {
        let current_timestamp = get_current_timestamp();

        #[rustfmt::skip]
        let row = sqlx::query(
            "
                SELECT
                    1
                FROM
                    `files`
                WHERE
                    `id` = ?
                        AND ( `expired_at` IS NULL OR `expired_at` > ? )
            ",
        )
        .bind(id.into())
        .bind(current_timestamp)
        .fetch_optional(&self.0.db)
        .await?;

        Ok(row.is_some())
    }

    /// Get the file metadata using an ID.
    pub async fn get_file_by_id(
        &self,
        id: impl Into<Uuid>,
    ) -> Result<Option<DatalithFile>, DatalithReadError> {
        let current_timestamp = get_current_timestamp();

        let id = id.into();

        // Keep the file alive while reading its metadata.
        let guard = OpenGuard::new(self.clone(), id).await;

        let row: Option<(i64, i64, String, String, bool)> = sqlx::query_as(
            "SELECT created_at, file_size, file_type, file_name, 0 FROM files
             WHERE id = ? AND expired_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&self.0.db)
        .await?;
        let row = if row.is_some() {
            row
        } else {
            sqlx::query_as(
                "UPDATE files SET expired_at = 0 WHERE id = ? AND expired_at > ?
                 RETURNING created_at, file_size, file_type, file_name, 1",
            )
            .bind(id)
            .bind(current_timestamp)
            .fetch_optional(&self.0.db)
            .await?
        };

        if let Some((created_at, file_size, file_type, file_name, is_temporary)) = row {
            let created_at = DateTime::from_timestamp_millis(created_at).unwrap();
            let file_type = Mime::from_str(&file_type).unwrap();

            let file = DatalithFile::new(
                self.clone(),
                guard,
                id,
                created_at,
                file_size as u64,
                file_type,
                file_name,
                is_temporary,
                false,
            );

            Ok(Some(file))
        } else {
            Ok(None)
        }
    }

    pub(crate) async fn get_file_by_hash(
        &self,
        hash: &[u8; 32],
    ) -> Result<Option<DatalithFile>, DatalithReadError> {
        let id: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM files WHERE hash = ? AND expired_at IS NULL")
                .bind(hash.as_slice())
                .fetch_optional(&self.0.db)
                .await?;

        if let Some((id,)) = id { self.get_file_by_id(id).await } else { Ok(None) }
    }

    /// List file IDs.
    pub async fn list_file_ids(
        &self,
        pagination_options: PaginationOptions<DatalithFileOrderBy>,
    ) -> Result<(Vec<Uuid>, Pagination), DatalithReadError> {
        let current_timestamp = get_current_timestamp();
        let (joins, order_by_components) = pagination_options.order_by.to_sql();
        let mut sql_join = String::new();
        let mut sql_order_by = String::new();
        let mut sql_limit_offset = String::new();
        SqlJoin::format_sqlite_join_clauses(&joins, &mut sql_join);
        SqlOrderByComponent::format_sqlite_order_by_components(
            &order_by_components,
            &mut sql_order_by,
        );
        pagination_options.to_sqlite_limit_offset(&mut sql_limit_offset);
        let mut tx = self.0.db.begin().await?;
        let (total_items,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM files WHERE expired_at IS NULL OR expired_at > ?")
                .bind(current_timestamp)
                .fetch_one(&mut *tx)
                .await?;
        let sql = format!(
            "SELECT id FROM files {sql_join} WHERE expired_at IS NULL OR expired_at > ? \
             {sql_order_by} {sql_limit_offset}",
        );
        let rows: Vec<(Uuid,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(current_timestamp)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        let pagination = Pagination::new()
            .items_per_page(pagination_options.items_per_page)
            .total_items(total_items as usize)
            .page(pagination_options.page);
        Ok((rows.into_iter().map(|(id,)| id).collect(), pagination))
    }
}

// Delete
impl Datalith {
    /// Remove a file by ID.
    /// Drop all related `DatalithFile` values before calling this function.
    #[inline]
    pub async fn delete_file_by_id(&self, id: impl Into<Uuid>) -> Result<bool, DatalithReadError> {
        let id = id.into();

        let guard = DeleteGuard::new(self.clone(), id).await;

        self.wait_for_opening_files(&guard).await?;

        self.delete_file_by_id_inner(id, guard).await
    }

    pub(crate) async fn wait_for_opening_files(
        &self,
        guard: &DeleteGuard,
    ) -> Result<(), DatalithReadError> {
        self.wait_for_opening_file_references(guard, 1).await
    }

    pub(crate) async fn wait_for_opening_file_references(
        &self,
        guard: &DeleteGuard,
        count: u64,
    ) -> Result<(), DatalithReadError> {
        let id = guard.id;

        let multiple = {
            #[rustfmt::skip]
            let result = sqlx::query(
                "
                    SELECT
                        1
                    FROM
                        `files`
                    WHERE
                        `id` = ?
                            AND `count` > ?
                ",
            )
            .bind(id)
            .bind(count as i64)
            .fetch_optional(&self.0.db)
            .await?;

            result.is_some()
        };

        if !multiple {
            // Wait until all readers have released the file.
            loop {
                let changed = self.0._file_changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if !self.0._file_lifecycle.lock().unwrap().opening.contains_key(&id) {
                    break;
                }
                changed.await;
            }
        }

        Ok(())
    }

    pub(crate) async fn delete_file_by_id_inner(
        &self,
        id: impl Into<Uuid>,
        guard: DeleteGuard,
    ) -> Result<bool, DatalithReadError> {
        let id = id.into();
        let _guard = guard;
        let mut tx = self.0.db.begin_with("BEGIN IMMEDIATE").await?;
        if sqlx::query("SELECT 1 FROM files WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .is_none()
        {
            return Ok(false);
        }
        let removed = match Self::release_file_references_in_transaction(&mut tx, id, 1).await {
            Ok(removed) => removed,
            Err(error)
                if error
                    .as_database_error()
                    .is_some_and(|error| error.is_foreign_key_violation()) =>
            {
                return Ok(false);
            },
            Err(error) => return Err(error.into()),
        };
        tx.commit().await?;
        if removed {
            self.remove_untracked_file(id).await?;
        }
        Ok(true)
    }

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

    pub(crate) async fn remove_untracked_file(&self, id: Uuid) -> Result<(), DatalithReadError> {
        let mut tx = self.0.db.begin().await?;
        let mapping: Option<(Uuid,)> = sqlx::query_as(
            "DELETE FROM blob_files WHERE file_id = ? AND NOT EXISTS (SELECT 1 FROM files WHERE \
             id = ?) RETURNING storage_id",
        )
        .bind(id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let storage_id = mapping.map_or(id, |(id,)| id);
        let tracked = sqlx::query(
            "SELECT 1 FROM files LEFT JOIN blob_files ON files.id = blob_files.file_id
             WHERE files.id = ? AND (blob_files.storage_id IS NULL OR blob_files.storage_id = \
             files.id)
             UNION ALL SELECT 1 FROM files JOIN blob_files ON files.id = blob_files.file_id WHERE \
             storage_id = ? LIMIT 1",
        )
        .bind(storage_id)
        .bind(storage_id)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        tx.commit().await?;
        if tracked {
            return Ok(());
        }
        let file_path =
            self.get_file_directory().await?.join(format!("{:x}", storage_id.as_u128()));
        // Cleanup can retry a failed file removal later.
        if let Err(error) = allow_not_found_error(fs::remove_file(file_path).await) {
            tracing::warn!(%id, %error, "cannot remove untracked file");
        }
        Ok(())
    }
}

async fn handle_file_type(
    file_type: Option<(Mime, FileTypeLevel)>,
    detect_file_type: impl Future<Output = Option<Mime>> + Sized,
) -> Result<Mime, DatalithWriteError> {
    if let Some((file_type, level)) = file_type {
        match level {
            FileTypeLevel::ExactMatch => {
                let detected_file_type = detect_file_type.await;

                if let Some(detected_file_type) = detected_file_type {
                    if file_type != detected_file_type {
                        return Err(DatalithWriteError::FileTypeInvalid {
                            file_type:          Box::new(detected_file_type),
                            expected_file_type: Box::new(file_type),
                        });
                    }

                    Ok(file_type)
                } else {
                    Ok(file_type)
                }
            },
            FileTypeLevel::Manual => Ok(file_type),
            FileTypeLevel::Fallback => {
                let detected_file_type = detect_file_type.await;

                Ok(detected_file_type.unwrap_or(file_type))
            },
        }
    } else {
        let detected_file_type = detect_file_type.await;

        Ok(detected_file_type.unwrap_or(DEFAULT_MIME_TYPE))
    }
}

pub(crate) async fn get_file_size_by_reader_and_copy_to_file(
    reader: impl AsyncRead + Unpin,
    file_path: impl AsRef<Path>,
    expected_reader_length: Option<u64>,
) -> Result<u64, DatalithWriteError> {
    copy_reader_to_file(reader, file_path.as_ref(), expected_reader_length, None).await
}

async fn get_file_size_and_hash_by_reader_and_copy_to_file(
    reader: impl AsyncRead + Unpin,
    file_path: impl AsRef<Path>,
    expected_reader_length: Option<u64>,
) -> Result<(u64, [u8; 32]), DatalithWriteError> {
    let mut hasher = Sha256::new();

    let file_size =
        copy_reader_to_file(reader, file_path.as_ref(), expected_reader_length, Some(&mut hasher))
            .await?;

    Ok((file_size, hasher.finalize().into()))
}

async fn copy_reader_to_file(
    mut reader: impl AsyncRead + Unpin,
    file_path: &Path,
    expected_reader_length: Option<u64>,
    mut hasher: Option<&mut Sha256>,
) -> Result<u64, DatalithWriteError> {
    let mut cleanup = TemporaryFileGuard::new(file_path);
    let mut file = File::create(file_path).await?;
    let mut file_size = 0u64;
    let mut retry_count = 0;
    let mut buffer =
        vec![0; expected_reader_length.map(calculate_buffer_size).unwrap_or(BUFFER_SIZE)];

    loop {
        let count = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == ErrorKind::Interrupted && retry_count < 5 => {
                retry_count += 1;
                continue;
            },
            Err(error) => return Err(error.into()),
        };
        retry_count = 0;
        file_size += count as u64;

        // Stop reading at once so that an endless reader cannot keep this upload busy.
        if let Some(expected_file_length) = expected_reader_length
            && file_size > expected_file_length
        {
            return Err(DatalithWriteError::FileLengthTooLarge {
                expected_file_length,
                actual_file_length: file_size,
            });
        }

        file.write_all(&buffer[..count]).await?;

        if let Some(hasher) = hasher.as_mut() {
            hasher.update(&buffer[..count]);
        }
    }

    file.flush().await?;
    file.sync_all().await?;
    cleanup.set_moved();
    Ok(file_size)
}
