#[cfg(feature = "image-convert")]
use std::sync::atomic::{AtomicU32, AtomicU8};
use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Debug, Formatter},
    future::Future,
    io,
    io::ErrorKind,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use chrono::prelude::*;
use educe::Educe;
use fs4::tokio::AsyncFileExt;
use mime::Mime;
use rdb_pagination::{prelude::*, Pagination, PaginationOptions, SqlJoin, SqlOrderByComponent};
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteQueryResult},
    Acquire, Pool, Row, Sqlite,
};
use tokio::{
    fs,
    fs::{File, OpenOptions},
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    task::JoinSet,
    time,
};
pub use uuid::Uuid;

use crate::{
    functions::{
        allow_not_found_error, calculate_buffer_size, detect_file_type_by_buffer,
        detect_file_type_by_path, get_current_timestamp, get_file_name, get_hash_by_buffer,
        get_hash_by_path, get_random_hash, BUFFER_SIZE,
    },
    guard::{DeleteGuard, OpenGuard, PutGuard, TemporaryFileGuard},
    DatalithCreateError, DatalithFile, DatalithReadError, DatalithWriteError, DEFAULT_MIME_TYPE,
};

/// The path to the SQLite DB file.
pub const PATH_DB_FILE: &str = "datalith.sqlite";
/// The path to the directory where all the handling files are located.
pub const PATH_TEMPORARY_FILE_DIRECTORY: &str = "datalith.temp";
/// The path to the directory where all stored files are located.
pub const PATH_FILE_DIRECTORY: &str = "datalith.files";

const DATABASE_VERSION: u32 = 1;
const TABLE_DB_INFORMATION: &str = "sys_db_information";

const FILE_READ_BUFFER_SIZE: usize = 64 * 1024;
const TEMPORARY_FILE_LIFESPAN: Duration = Duration::from_secs(60);

#[cfg(feature = "image-convert")]
const MAX_IMAGE_RESOLUTION: u32 = 50_000_000; // 50MP
#[cfg(feature = "image-convert")]
const MAX_IMAGE_RESOLUTION_MULTIPLIER: u8 = 3; // 1x, 2x, 3x

/// A struct that defines the ordering options for querying files.
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
    environment:                                 PathBuf,
    _create_time:                                DateTime<Local>,
    _version:                                    u32,
    pub(crate) _uploading_files:                 Mutex<HashSet<[u8; 32]>>,
    pub(crate) _opening_files:                   Mutex<HashMap<Uuid, usize>>,
    pub(crate) _deleting_files:                  Mutex<HashSet<Uuid>>,
    _sql_file:                                   File,
    pub(crate) _file_read_buffer_size:           AtomicUsize,
    pub(crate) _temporary_file_lifespan:         AtomicU64,
    #[cfg(feature = "image-convert")]
    pub(crate) _max_image_resolution:            AtomicU32,
    #[cfg(feature = "image-convert")]
    pub(crate) _max_image_resolution_multiplier: AtomicU8,
}

/// The Datalith file storage center.
#[derive(Clone)]
pub struct Datalith(pub(crate) Arc<DatalithInner>);

impl Debug for Datalith {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Debug::fmt(self.0.as_ref(), f)
    }
}

impl Datalith {
    /// Retrieve the root path of this Datalith.
    #[inline]
    pub fn get_environment(&self) -> &Path {
        self.0.environment.as_path()
    }

    /// Retrieve the size of the file read buffer.
    #[inline]
    pub fn get_file_read_buffer_size(&self) -> usize {
        self.0._file_read_buffer_size.load(Ordering::Relaxed)
    }

    /// Set the size (in bytes) of the file read buffer.
    ///
    /// The minimum size is **512 KiB**. The maximum size is **64 MiB**.
    #[inline]
    pub fn set_file_read_buffer_size(&self, mut size: usize) {
        size = size.clamp(512 * 1024, 64 * 1024 * 1024);

        self.0._file_read_buffer_size.swap(size, Ordering::Relaxed);
    }

    /// Retrieve the lifespan for each of the uploaded temporary files.
    #[inline]
    pub fn get_temporary_file_lifespan(&self) -> Duration {
        let milli_secs = self.0._temporary_file_lifespan.load(Ordering::Relaxed);

        Duration::from_millis(milli_secs)
    }

    /// Set the lifespan for each of the uploaded temporary files.
    ///
    /// The minimum lifespan is **100 milliseconds**. The maximum lifespan is **10000 hours**.
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
        Ok(self.get_file_directory().await?.join(format!("{:x}", id.as_u128())))
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

// UP / Down
impl Datalith {
    /// Create a Datalith file storage center.
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

                // should not panic because `environment_path_ref` has just been created
                fs::canonicalize(environment_path_ref).await.unwrap()
            },
            Err(error) => return Err(error.into()),
        };

        let sql_file_path = environment_path.join(PATH_DB_FILE);

        let mut sql_options = SqliteConnectOptions::new().filename(sql_file_path.as_path());

        match fs::metadata(&sql_file_path).await {
            Ok(metadata) => {
                if !metadata.is_file() {
                    return Err(DatalithCreateError::IOError(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{sql_file_path:?} exists but it is not a file"),
                    )));
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                sql_options = sql_options.create_if_missing(true);
            },
            Err(error) => return Err(error.into()),
        }

        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(num_cpus::get() as u32 * 10)
            .connect_with(sql_options)
            .await?;

        let sql_file = {
            let sql_file = OpenOptions::new().write(true).open(sql_file_path.as_path()).await?;

            match sql_file.try_lock_exclusive() {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Err(DatalithCreateError::AlreadyRun);
                },
                Err(error) => return Err(error.into()),
            }

            sql_file
        };

        let (version, create_time) = Self::initial_with_migration(&pool).await?;

        let uploading_files = Mutex::new(HashSet::new());
        let opening_files = Mutex::new(HashMap::new());
        let deleting_files = Mutex::new(HashSet::new());

        let datalith = Self(Arc::new(DatalithInner {
            db:                                                                 pool,
            environment:                                                        environment_path,
            _create_time:                                                       create_time,
            _version:                                                           version,
            _uploading_files:                                                   uploading_files,
            _opening_files:                                                     opening_files,
            _deleting_files:                                                    deleting_files,
            _sql_file:                                                          sql_file,
            _file_read_buffer_size:                                             AtomicUsize::new(
                FILE_READ_BUFFER_SIZE,
            ),
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

        // clear temp
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
    ) -> Result<(u32, DateTime<Local>), DatalithCreateError> {
        let (version, create_time) = Self::initial_db_or_fetch_information(pool).await?;

        if DATABASE_VERSION < version {
            return Err(DatalithCreateError::DatabaseTooNewError {
                app_db_version:     DATABASE_VERSION,
                current_db_version: version,
            });
        }

        if DATABASE_VERSION > version {
            for upgrade_version in (version + 1)..=DATABASE_VERSION {
                #[allow(clippy::match_single_binding)]
                match upgrade_version {
                    2 => {
                        // TODO
                    },
                    _ => {
                        return Err(DatalithCreateError::DatabaseTooOldError {
                            app_db_version:     DATABASE_VERSION,
                            current_db_version: version,
                        });
                    },
                }
            }
        }

        Ok((version, create_time))
    }

    async fn initial_db_or_fetch_information(
        pool: &Pool<Sqlite>,
    ) -> Result<(u32, DateTime<Local>), DatalithCreateError> {
        let mut conn = pool.acquire().await?;

        let mut tx = conn.begin().await?;

        let result = sqlx::query(&format!(
            "
                CREATE TABLE `{TABLE_DB_INFORMATION}` (
                    `key`   TEXT PRIMARY KEY NOT NULL,
                    `value` TEXT
                )
            "
        ))
        .execute(&mut *tx)
        .await;

        let exist = Self::check_create_table_already_exist(result)?;

        let (version, create_time) = if !exist {
            let create_time = Local::now();
            let create_time_rfc = create_time.to_rfc3339();

            #[rustfmt::skip]
            sqlx::query(&format!(
                "
                    INSERT INTO `{TABLE_DB_INFORMATION}`
                        VALUES
                            ('version', '{DATABASE_VERSION}'),
                            ('create_time', '{create_time_rfc}')
                "
            ))
            .execute(&mut *tx)
            .await?;

            let schema_sql = include_str!("sql/schema.sql");

            for sql in schema_sql.split(";\n") {
                let sql = sql.trim();

                if sql.is_empty() {
                    continue;
                }

                sqlx::query(sql).execute(&mut *tx).await?;
            }

            tx.commit().await?;

            (DATABASE_VERSION, create_time)
        } else {
            tx.commit().await?;

            let version = {
                #[rustfmt::skip]
                let row = sqlx::query(&format!(
                    "
                        SELECT
                            `value`
                        FROM
                            `{TABLE_DB_INFORMATION}`
                        WHERE
                            `key` = 'version'
                    "
                ))
                .fetch_one(&mut *conn)
                .await?;

                row.get::<String, _>(0).parse().unwrap()
            };

            let create_time = {
                #[rustfmt::skip]
                let row = sqlx::query(&format!(
                    "
                        SELECT
                            `value`
                        FROM
                            `{TABLE_DB_INFORMATION}`
                        WHERE
                            `key` = 'create_time'
                    "
                ))
                .fetch_one(&mut *conn)
                .await?;

                let create_time_rfc = row.get::<String, _>(0);

                DateTime::parse_from_rfc3339(&create_time_rfc).unwrap().into()
            };

            (version, create_time)
        };

        Ok((version, create_time))
    }

    #[inline]
    fn check_create_table_already_exist(
        result: Result<SqliteQueryResult, sqlx::Error>,
    ) -> Result<bool, sqlx::Error> {
        if let Err(error) = result {
            if let sqlx::Error::Database(ref error) = error {
                if let Some(code) = error.code() {
                    if code.as_ref() == "1" {
                        return Ok(true);
                    }
                }
            }

            return Err(error);
        }

        Ok(false)
    }

    /// Close the Datalith file storage center.
    #[inline]
    pub async fn close(self) {
        self.0.db.close().await;
    }

    /// Close the Datalith file storage center and remove the entire database and associated files.
    #[inline]
    pub async fn drop_datalith(self) -> Result<(), io::Error> {
        self.0.db.close().await;

        // remove associated files
        allow_not_found_error(fs::remove_file(self.0.environment.join(PATH_DB_FILE)).await)?;
        allow_not_found_error(
            fs::remove_dir_all(self.0.environment.join(PATH_TEMPORARY_FILE_DIRECTORY)).await,
        )?;
        allow_not_found_error(
            fs::remove_dir_all(self.0.environment.join(PATH_FILE_DIRECTORY)).await,
        )?;

        match fs::read_dir(self.0.environment.as_path()).await {
            Ok(mut dir) => {
                if dir.next_entry().await?.is_none() {
                    // if the environment directory is empty, remove it
                    allow_not_found_error(fs::remove_dir(self.0.environment.as_path()).await)?;
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }

        Ok(())
    }
}

/// Defines how to handle MIME type comparison when putting files.
#[derive(Debug, Clone, Copy)]
pub enum FileTypeLevel {
    /// The detected file type must exactly match the provided file type.
    ExactMatch,
    /// The file type detection is bypassed, and the provided file type is used directly.
    Manual,
    /// The detected file type is preferred, but if detection fails, the provided file type is used as a fallback.
    Fallback,
}

// Permanent Upload
impl Datalith {
    /// Input a file into Datalith using a buffer.
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
        let id = Uuid::new_v4(); // we can assume this id cannot be deleted
        let created_at = Local::now();
        let file_size = file_data.len() as i64;
        let file_type =
            handle_file_type(file_type, async { detect_file_type_by_buffer(file_data).await })
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
        .bind(file_size)
        .bind(file_type.essence_str())
        .bind(file_name.as_str())
        .bind(expired_at)
        .execute(&mut *tx)
        .await?;

        debug_assert!(result.rows_affected() > 0);

        let file_path = self.get_file_path(id).await?;

        // protect this id before actually store in the DB
        let open_guard = OpenGuard::new(self.clone(), id).await;

        fs::write(file_path, file_data).await?;

        tx.commit().await?;

        let file = DatalithFile::new(
            self.clone(),
            open_guard,
            id,
            created_at,
            file_size as u64,
            file_type,
            file_name,
            expired_at.is_some(),
            true,
        );

        Ok(file)
    }

    /// Input a file into Datalith using a file path.
    pub async fn put_file_by_path(
        &self,
        file_path: impl AsRef<Path>,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let file_path = file_path.as_ref();

        let hash = get_hash_by_path(file_path).await?;

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
            self.put_file_by_path_inner(hash, file_path, file_name, file_type, false).await
        }
    }

    async fn put_file_by_path_inner(
        &self,
        hash: [u8; 32],
        file_path: &Path,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
        temporary: bool,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let id = Uuid::new_v4(); // we can assume this id cannot be deleted
        let created_at = Local::now();
        let file_metadata = fs::metadata(file_path).await?;
        let file_size = file_metadata.len() as i64;
        let file_type = handle_file_type(file_type, async {
            match fs::canonicalize(file_path).await {
                Ok(file_path) => detect_file_type_by_path(file_path, true).await,
                Err(_) => None,
            }
        })
        .await?;
        let file_name = if let Some(file_name) = file_name {
            get_file_name(Some(file_name), created_at, &file_type)
        } else if let Some(file_name) = file_path.file_name() {
            file_name.to_string_lossy().into_owned()
        } else {
            unreachable!();
        };
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
        .bind(file_size)
        .bind(file_type.essence_str())
        .bind(file_name.as_str())
        .bind(expired_at)
        .execute(&mut *tx)
        .await?;

        debug_assert!(result.rows_affected() > 0);

        let original_file_path = file_path;
        let file_path = self.get_file_path(id).await?;

        // protect this id before actually store in the DB
        let open_guard = OpenGuard::new(self.clone(), id).await;

        fs::copy(original_file_path, file_path).await?;

        tx.commit().await?;

        let file = DatalithFile::new(
            self.clone(),
            open_guard,
            id,
            created_at,
            file_size as u64,
            file_type,
            file_name,
            expired_at.is_some(),
            true,
        );

        Ok(file)
    }

    /// Input a file into Datalith using a reader.
    pub async fn put_file_by_reader(
        &self,
        reader: impl AsyncRead + Unpin,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
        expected_reader_length: Option<u64>,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let temporary_file_path = self.get_temporary_file_path(Uuid::new_v4()).await?;

        let (file_size, hash) = get_file_size_and_hash_by_reader_and_copy_to_file(
            reader,
            temporary_file_path.as_path(),
            expected_reader_length,
        )
        .await?;
        let mut file_guard = TemporaryFileGuard::new(temporary_file_path.as_path());

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
        let file_path = self.get_file_path(id).await?;

        // protect this id before actually store in the DB
        let open_guard = OpenGuard::new(self.clone(), id).await;

        fs::rename(original_file_path, file_path).await?;
        file_guard.set_moved();

        tx.commit().await?;

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

// Temporary Upload
impl Datalith {
    /// Temporarily input a file into Datalith using a buffer.
    ///
    /// The term `temporarily` means the file can be retrieved using the `get_file_by_id` function only once. After that, it cannot be retrieved again.
    pub async fn put_file_by_buffer_temporarily(
        &self,
        buffer: impl AsRef<[u8]>,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let hash = get_random_hash(); // we can assume this hash will not be duplicated

        self.put_file_by_buffer_inner(hash, buffer.as_ref(), file_name, file_type, true).await
    }

    /// Temporarily input a file into Datalith using a file path.
    ///
    /// The term `temporarily` means the file can be retrieved using the `get_file_by_id` function only once. After that, it cannot be retrieved again.
    pub async fn put_file_by_path_temporarily(
        &self,
        file_path: impl AsRef<Path>,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let hash = get_random_hash(); // we can assume this hash will not be duplicated

        self.put_file_by_path_inner(hash, file_path.as_ref(), file_name, file_type, true).await
    }

    /// Temporarily input a file into Datalith using a reader.
    ///
    /// The term `temporarily` means the file can be retrieved using the `get_file_by_id` function only once. After that, it cannot be retrieved again.
    pub async fn put_file_by_reader_temporarily(
        &self,
        reader: impl AsyncRead + Unpin,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
        expected_reader_length: Option<u64>,
    ) -> Result<DatalithFile, DatalithWriteError> {
        let temporary_file_path = self.get_temporary_file_path(Uuid::new_v4()).await?;

        let hash = get_random_hash(); // we can assume this hash will not be duplicated

        let file_size = get_file_size_by_reader_and_copy_to_file(
            reader,
            temporary_file_path.as_path(),
            expected_reader_length,
        )
        .await?;
        let mut file_guard = TemporaryFileGuard::new(temporary_file_path.as_path());

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

// Clean Up
impl Datalith {
    /// Clear expired files.
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

        let mut tasks = JoinSet::new();

        for (id,) in rows {
            let datalith = self.clone();

            tasks.spawn(async move {
                #[rustfmt::skip]
                sqlx::query(
                    "
                        DELETE FROM
                            `resources`
                        WHERE
                            `file_id` = ?
                    ",
                )
                .bind(id)
                .execute(&datalith.0.db).await?;

                // files are all temporary, so the count should always be 1
                // we don't need a loop to ensure that if a file is uploaded multiple times, it should be deleted multiple times
                time::timeout(timeout, datalith.delete_file_by_id(id))
                    .await
                    .unwrap_or_else(|_| Ok(false))
            });
        }

        let mut counter = 0usize;

        while let Some(result) = tasks.join_next().await {
            if let Ok(result) = result.unwrap() {
                if result {
                    counter += 1;
                }
            }
        }

        Ok(counter)
    }

    /// Clear untracked files in the file system.
    pub async fn clear_untracked_files(&self) -> Result<usize, DatalithReadError> {
        let file_directory = self.get_file_directory().await?;

        let mut counter = 0usize;

        for dir in file_directory.read_dir()? {
            let entry = dir?;

            let path = entry.path();

            let metadata = fs::metadata(path.as_path()).await?;

            if metadata.is_dir() {
                allow_not_found_error(fs::remove_dir_all(path).await)?;
                counter += 1;

                continue;
            } else if metadata.is_symlink() {
                allow_not_found_error(fs::remove_file(path).await)?;
                counter += 1;

                continue;
            }

            let file_id = if let Some(file_id) = path
                .file_name()
                .and_then(|e| e.to_str())
                .and_then(|e| u128::from_str_radix(e, 16).ok())
                .map(Uuid::from_u128)
            {
                file_id
            } else {
                allow_not_found_error(fs::remove_file(path.as_path()).await)?;
                counter += 1;

                continue;
            };

            if !self.check_file_exist(file_id).await? {
                {
                    let opening_files = self.0._opening_files.lock().unwrap();

                    if opening_files.contains_key(&file_id) {
                        continue;
                    }
                }

                allow_not_found_error(fs::remove_file(path.as_path()).await)?;
                counter += 1;
            }
        }

        Ok(counter)
    }
}

// Download
impl Datalith {
    /// Check whether the file exists or not.
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

    /// Retrieve the file metadata using an ID.
    pub async fn get_file_by_id(
        &self,
        id: impl Into<Uuid>,
    ) -> Result<Option<DatalithFile>, DatalithReadError> {
        let current_timestamp = get_current_timestamp();

        let id = id.into();

        // protect ID
        let guard = OpenGuard::new(self.clone(), id).await;

        // wait for deleting processes
        loop {
            {
                let deleting_files = self.0._deleting_files.lock().unwrap();

                if !deleting_files.contains(&id) {
                    break;
                }
            }

            time::sleep(Duration::from_millis(10)).await;
        }

        let is_temporary = {
            let result = sqlx::query(
                "
                    UPDATE
                        `files`
                    SET
                        `expired_at` = ?
                    WHERE
                        `id` = ?
                            AND `expired_at` > ?
                ",
            )
            .bind(current_timestamp)
            .bind(id)
            .bind(current_timestamp)
            .execute(&self.0.db)
            .await?;

            result.rows_affected() > 0
        };

        #[rustfmt::skip]
        let row: Option<(i64, u64, String, String)> = sqlx::query_as(
            "
                SELECT
                    `created_at`,
                    `file_size`,
                    `file_type`,
                    `file_name`
                FROM
                    `files`
                WHERE
                    `id` = ?
                        AND (`expired_at` IS NULL OR `expired_at` = ?)
            ",
        )
        .bind(id)
        .bind(current_timestamp)
        .fetch_optional(&self.0.db)
        .await?;

        if let Some((created_at, file_size, file_type, file_name)) = row {
            let created_at = DateTime::from_timestamp_millis(created_at).unwrap();
            let file_type = Mime::from_str(&file_type).unwrap();

            let file = DatalithFile::new(
                self.clone(),
                guard,
                id,
                created_at,
                file_size,
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
        let current_timestamp = get_current_timestamp();

        #[rustfmt::skip]
        #[allow(clippy::type_complexity)]
        let row: Option<(Uuid, i64, u64, String, String, Option<i64>)> = sqlx::query_as(
            "
                SELECT
                    `id`,
                    `created_at`,
                    `file_size`,
                    `file_type`,
                    `file_name`,
                    `expired_at`
                FROM
                    `files`
                WHERE
                    `hash` = ?
                        AND ( `expired_at` IS NULL OR `expired_at` > ? )
            ",
        )
        .bind(hash.to_vec())
        .bind(current_timestamp)
        .fetch_optional(&self.0.db)
        .await?;

        if let Some((id, created_at, file_size, file_type, file_name, expired_at)) = row {
            // protect ID
            let guard = OpenGuard::new(self.clone(), id).await;

            // check deleting
            {
                let deleting_files = self.0._deleting_files.lock().unwrap();

                if deleting_files.contains(&id) {
                    return Ok(None);
                }
            }

            let created_at = DateTime::from_timestamp_millis(created_at).unwrap();
            let file_type = Mime::from_str(&file_type).unwrap();
            let is_temporary = expired_at.is_some();

            if is_temporary {
                #[rustfmt::skip]
                sqlx::query(
                    "
                        UPDATE
                            `files`
                        SET
                            `expired_at` = ?
                        WHERE
                            `id` = ?
                    ",
                )
                .bind(current_timestamp)
                .bind(id)
                .execute(&self.0.db)
                .await?;
            }

            let file = DatalithFile::new(
                self.clone(),
                guard,
                id,
                created_at,
                file_size,
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

    /// List file IDs.
    pub async fn list_file_ids(
        &self,
        mut pagination_options: PaginationOptions<DatalithFileOrderBy>,
    ) -> Result<(Vec<Uuid>, Pagination), DatalithReadError> {
        loop {
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

            let total_items = {
                let row: (u32,) = {
                    #[rustfmt::skip]
                    let query = sqlx::query_as(
                        "
                            SELECT
                                COUNT(*)
                            FROM
                                `files`
                        "
                    );

                    query.fetch_one(&mut *tx).await?
                };

                row.0
            };

            let rows: Vec<(Uuid,)> = {
                #[rustfmt::skip]
                let sql = format!(
                    "
                        SELECT
                            `id`
                        FROM
                            `files`
                        {sql_join}
                        WHERE
                            (`expired_at` IS NULL OR `expired_at` > ?)
                        {sql_order_by}
                        {sql_limit_offset}
                    "
                );

                let current_timestamp = get_current_timestamp();

                let query = sqlx::query_as(&sql).bind(current_timestamp);

                query.fetch_all(&mut *tx).await?
            };

            let total_items = total_items as usize;

            drop(tx);

            let pagination = Pagination::new()
                .items_per_page(pagination_options.items_per_page)
                .total_items(total_items)
                .page(pagination_options.page);

            if rows.is_empty() {
                if total_items > 0 && pagination_options.page > 1 {
                    pagination_options.page = pagination.get_total_pages();

                    continue;
                } else {
                    return Ok((Vec::new(), pagination));
                }
            }

            let ids = rows.into_iter().map(|(id,)| id).collect::<Vec<Uuid>>();

            return Ok((ids, pagination));
        }
    }
}

// Delete
impl Datalith {
    /// Remove a file using an ID. The related `DatalithFile` instances should be dropped before calling this function.
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
                            AND `count` > 1
                ",
            )
            .bind(id)
            .fetch_optional(&self.0.db)
            .await?;

            result.is_some()
        };

        if !multiple {
            // wait for all instances to be dropped
            loop {
                {
                    let opening_files = self.0._opening_files.lock().unwrap();

                    if !opening_files.contains_key(&id) {
                        break;
                    }
                }

                time::sleep(Duration::from_millis(10)).await;
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

        #[rustfmt::skip]
        let result = sqlx::query(
            "
                UPDATE
                    `files`
                SET
                    `count` = `count` - 1
                WHERE
                    `id` = ?
                        AND `count` > 1
            ",
        )
        .bind(id)
        .execute(&self.0.db)
        .await?;

        if result.rows_affected() > 0 {
            return Ok(true);
        }

        let mut tx = self.0.db.begin().await?;

        #[rustfmt::skip]
        let result = sqlx::query(
            "
                DELETE FROM
                    `files`
                WHERE
                    `id` = ?
                        AND `count` = 1
            ",
        )
        .bind(id)
        .execute(&mut *tx)
        .await;

        match result {
            Ok(result) => {
                if result.rows_affected() == 0 {
                    return Ok(false);
                }
            },
            Err(error) => {
                if let Some(error) = error.as_database_error() {
                    if let Some(code) = error.code() {
                        if code == "787" {
                            return Ok(false);
                        }
                    }
                }

                return Err(error.into());
            },
        }

        let file_path = self.get_file_path(id).await?;

        allow_not_found_error(fs::remove_file(file_path).await)?;

        tx.commit().await?;

        Ok(true)
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
                            file_type:          detected_file_type,
                            expected_file_type: file_type,
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
    mut reader: impl AsyncRead + Unpin,
    file_path: impl AsRef<Path>,
    expected_reader_length: Option<u64>,
) -> Result<u64, DatalithWriteError> {
    let file_path = file_path.as_ref();

    let mut file = File::create(file_path).await?;

    // copy the data

    let mut file_size = 0u64;

    let mut retry_count = 0;

    if let Some(expected_reader_length) = expected_reader_length {
        let mut buffer = vec![0; calculate_buffer_size(expected_reader_length)];

        loop {
            let c = match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(c) => c,
                Err(error) if error.kind() == ErrorKind::Interrupted => {
                    retry_count += 1;

                    if retry_count > 5 {
                        return Err(error.into());
                    }

                    continue;
                },
                Err(error) => {
                    fs::remove_file(file_path).await?;
                    return Err(error.into());
                },
            };

            match file.write_all(&buffer[..c]).await {
                Ok(_) => (),
                Err(error) => {
                    fs::remove_file(file_path).await?;
                    return Err(error.into());
                },
            }

            file_size += c as u64;

            if file_size > expected_reader_length {
                fs::remove_file(file_path).await?;

                // read the remaining data
                loop {
                    match reader.read(&mut buffer).await {
                        Ok(0) => break,
                        Ok(c) => {
                            file_size += c as u64;
                        },
                        Err(error) if error.kind() == ErrorKind::Interrupted => {
                            retry_count += 1;

                            if retry_count > 5 {
                                return Err(error.into());
                            }

                            continue;
                        },
                        Err(error) => {
                            return Err(error.into());
                        },
                    }

                    retry_count = 0;
                }

                return Err(DatalithWriteError::FileLengthTooLarge {
                    expected_file_length: expected_reader_length,
                    actual_file_length:   file_size,
                });
            }

            retry_count = 0;
        }
    } else {
        let mut buffer = vec![0; BUFFER_SIZE];

        loop {
            let c = match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(c) => c,
                Err(error) if error.kind() == ErrorKind::Interrupted => {
                    retry_count += 1;

                    if retry_count > 5 {
                        return Err(error.into());
                    }

                    continue;
                },
                Err(error) => {
                    fs::remove_file(file_path).await?;
                    return Err(error.into());
                },
            };

            match file.write_all(&buffer[..c]).await {
                Ok(_) => (),
                Err(error) => {
                    fs::remove_file(file_path).await?;
                    return Err(error.into());
                },
            }

            file_size += c as u64;

            retry_count = 0;
        }
    }

    Ok(file_size)
}

async fn get_file_size_and_hash_by_reader_and_copy_to_file(
    mut reader: impl AsyncRead + Unpin,
    file_path: impl AsRef<Path>,
    expected_reader_length: Option<u64>,
) -> Result<(u64, [u8; 32]), DatalithWriteError> {
    let file_path = file_path.as_ref();

    let mut hasher = Sha256::new();
    let mut file = File::create(file_path).await?;

    // copy the data and calculate the hash value

    let mut file_size = 0u64;

    let mut retry_count = 0;

    if let Some(expected_reader_length) = expected_reader_length {
        let mut buffer = vec![0; calculate_buffer_size(expected_reader_length)];

        loop {
            let c = match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(c) => c,
                Err(error) if error.kind() == ErrorKind::Interrupted => {
                    retry_count += 1;

                    if retry_count > 5 {
                        return Err(error.into());
                    }

                    continue;
                },
                Err(error) => {
                    fs::remove_file(file_path).await?;
                    return Err(error.into());
                },
            };

            match file.write_all(&buffer[..c]).await {
                Ok(_) => (),
                Err(error) => {
                    fs::remove_file(file_path).await?;
                    return Err(error.into());
                },
            }

            hasher.update(&buffer[..c]);

            file_size += c as u64;

            if file_size > expected_reader_length {
                fs::remove_file(file_path).await?;

                // read the remaining data
                loop {
                    match reader.read(&mut buffer).await {
                        Ok(0) => break,
                        Ok(c) => {
                            file_size += c as u64;
                        },
                        Err(error) if error.kind() == ErrorKind::Interrupted => {
                            retry_count += 1;

                            if retry_count > 5 {
                                return Err(error.into());
                            }

                            continue;
                        },
                        Err(error) => {
                            return Err(error.into());
                        },
                    }

                    retry_count = 0;
                }

                return Err(DatalithWriteError::FileLengthTooLarge {
                    expected_file_length: expected_reader_length,
                    actual_file_length:   file_size,
                });
            }

            retry_count = 0;
        }
    } else {
        let mut buffer = vec![0; BUFFER_SIZE];

        loop {
            let c = match reader.read(&mut buffer).await {
                Ok(0) => break,
                Ok(c) => c,
                Err(error) if error.kind() == ErrorKind::Interrupted => {
                    retry_count += 1;

                    if retry_count > 5 {
                        return Err(error.into());
                    }

                    continue;
                },
                Err(error) => {
                    fs::remove_file(file_path).await?;
                    return Err(error.into());
                },
            };

            match file.write_all(&buffer[..c]).await {
                Ok(_) => (),
                Err(error) => {
                    fs::remove_file(file_path).await?;
                    return Err(error.into());
                },
            }

            hasher.update(&buffer[..c]);

            file_size += c as u64;

            retry_count = 0;
        }
    }

    Ok((file_size, hasher.finalize().into()))
}
