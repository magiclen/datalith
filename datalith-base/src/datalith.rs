use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::prelude::*;
use short_crypt::ShortCrypt;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteQueryResult},
    Acquire, Pool, Row, Sqlite,
};
use tokio::fs;

use crate::DatalithCreateError;

pub const PATH_DB_FILE: &str = "datalith.sqlite";
pub const PATH_TEMPORARY_FILE_DIRECTORY: &str = "temp";

const DATABASE_VERSION: u32 = 1;
const TABLE_DB_INFORMATION: &str = "sys_db_information";

#[derive(Debug)]
struct DatalithInner {
    db:           Pool<Sqlite>,
    environment:  PathBuf,
    _create_time: DateTime<Utc>,
    _version:     u32,
    short_crypt:  ShortCrypt,
}

/// The Datalith file manager.
#[derive(Debug, Clone)]
pub struct Datalith(Arc<DatalithInner>);

impl Datalith {
    #[inline]
    pub async fn drop_database(self) -> Result<(), io::Error> {
        self.0.db.close().await;
        #[inline]
        fn allow_not_found_error(result: io::Result<()>) -> io::Result<()> {
            match result {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        }

        allow_not_found_error(fs::remove_file(self.0.environment.join(PATH_DB_FILE)).await)?;
        allow_not_found_error(
            fs::remove_dir_all(self.0.environment.join(PATH_TEMPORARY_FILE_DIRECTORY)).await,
        )?;

        match fs::read_dir(self.0.environment.as_path()).await {
            Ok(mut dir) => {
                if dir.next_entry().await?.is_none() {
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

        let (version, create_time) = initial_with_migration(&pool).await?;

        let short_crypt = ShortCrypt::new(format!("datalith-{}", create_time.timestamp_millis()));

        Ok(Self(Arc::new(DatalithInner {
            db: pool,
            environment: environment_path,
            _create_time: create_time,
            _version: version,
            short_crypt,
        })))
    }
}

async fn initial_with_migration(
    pool: &Pool<Sqlite>,
) -> Result<(u32, DateTime<Utc>), DatalithCreateError> {
    let (version, create_time) = initial_db_or_fetch_information(pool).await?;

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
) -> Result<(u32, DateTime<Utc>), DatalithCreateError> {
    let mut conn = pool.acquire().await?;

    let mut tx = conn.begin().await?;

    let result = sqlx::query(&format!(
        "
            CREATE TABLE {TABLE_DB_INFORMATION} (
                `key`   TEXT PRIMARY KEY NOT NULL,
                `value` TEXT
            )
        "
    ))
    .execute(&mut *tx)
    .await;

    let exist = check_create_table_already_exist(result)?;

    let (version, create_time) = if !exist {
        let create_time = Utc::now();
        let create_time_rfc = create_time.to_rfc3339();

        sqlx::query(&format!(
            "
                INSERT INTO {TABLE_DB_INFORMATION}
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
            let row = sqlx::query(&format!(
                "
                    SELECT
                        value
                    FROM
                        {TABLE_DB_INFORMATION}
                    WHERE
                        key = 'version'
                "
            ))
            .fetch_one(&mut *conn)
            .await?;

            row.get::<String, _>(0).parse().unwrap()
        };

        let create_time = {
            let row = sqlx::query(&format!(
                "
                    SELECT
                        value
                    FROM
                        {TABLE_DB_INFORMATION}
                    WHERE
                        key = 'create_time'
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
