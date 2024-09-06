mod datalith_resource;

use std::{path::Path, str::FromStr};

use chrono::prelude::*;
pub use datalith_resource::*;
use educe::Educe;
use mime::Mime;
use rdb_pagination::{
    prelude::*, OrderByOptions, OrderMethod, Pagination, PaginationOptions, SqlJoin,
    SqlOrderByComponent,
};
use tokio::io::AsyncRead;
use uuid::Uuid;

use crate::{
    functions::{get_current_timestamp, get_file_name},
    guard::DeleteGuard,
    Datalith, DatalithFile, DatalithReadError, DatalithWriteError, FileTypeLevel,
};

/// A struct that defines the ordering options for querying resources.
#[derive(Debug, Clone, Educe, OrderByOptions)]
#[educe(Default)]
#[orderByOptions(name = resources)]
pub struct DatalithResourceOrderBy {
    #[educe(Default = 102)]
    #[orderByOptions((resources, id), unique)]
    pub id:         OrderMethod,
    #[educe(Default = -101)]
    #[orderByOptions((resources, created_at))]
    pub created_at: OrderMethod,
}

// Upload
impl Datalith {
    /// Input a resource into Datalith using a buffer.
    #[inline]
    pub async fn put_resource_by_buffer(
        &self,
        buffer: impl AsRef<[u8]>,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithResource, DatalithWriteError> {
        let file_name = file_name.map(|e| e.into());

        let file = self.put_file_by_buffer(buffer, file_name.clone(), file_type.clone()).await?;

        self.put_resource(file, file_name, file_type).await
    }

    /// Temporarily input a resource into Datalith using a buffer.
    #[inline]
    pub async fn put_resource_by_buffer_temporarily(
        &self,
        buffer: impl AsRef<[u8]>,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithResource, DatalithWriteError> {
        let file_name = file_name.map(|e| e.into());

        let file = self
            .put_file_by_buffer_temporarily(buffer, file_name.clone(), file_type.clone())
            .await?;

        self.put_resource(file, file_name, file_type).await
    }

    /// Input a resource into Datalith using a file path.
    #[inline]
    pub async fn put_resource_by_path(
        &self,
        file_path: impl AsRef<Path>,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithResource, DatalithWriteError> {
        let file_name = file_name.map(|e| e.into());

        let file = self.put_file_by_path(file_path, file_name.clone(), file_type.clone()).await?;

        self.put_resource(file, file_name, file_type).await
    }

    /// Temporarily input a resource into Datalith using a file path.
    #[inline]
    pub async fn put_resource_by_path_temporarily(
        &self,
        file_path: impl AsRef<Path>,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithResource, DatalithWriteError> {
        let file_name = file_name.map(|e| e.into());

        let file = self
            .put_file_by_path_temporarily(file_path, file_name.clone(), file_type.clone())
            .await?;

        self.put_resource(file, file_name, file_type).await
    }

    /// Input a resource into Datalith using a reader.
    #[inline]
    pub async fn put_resource_by_reader(
        &self,
        reader: impl AsyncRead + Unpin,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
        expected_reader_length: Option<u64>,
    ) -> Result<DatalithResource, DatalithWriteError> {
        let file_name = file_name.map(|e| e.into());

        let file = self
            .put_file_by_reader(
                reader,
                file_name.clone(),
                file_type.clone(),
                expected_reader_length,
            )
            .await?;

        self.put_resource(file, file_name, file_type).await
    }

    /// Temporarily input a resource into Datalith using a reader.
    #[inline]
    pub async fn put_resource_by_reader_temporarily(
        &self,
        reader: impl AsyncRead + Unpin,
        file_name: Option<impl Into<String>>,
        file_type: Option<(Mime, FileTypeLevel)>,
        expected_reader_length: Option<u64>,
    ) -> Result<DatalithResource, DatalithWriteError> {
        let file_name = file_name.map(|e| e.into());

        let file = self
            .put_file_by_reader_temporarily(
                reader,
                file_name.clone(),
                file_type.clone(),
                expected_reader_length,
            )
            .await?;

        self.put_resource(file, file_name, file_type).await
    }

    async fn put_resource(
        &self,
        file: DatalithFile,
        file_name: Option<String>,
        file_type: Option<(Mime, FileTypeLevel)>,
    ) -> Result<DatalithResource, DatalithWriteError> {
        macro_rules! recover_file {
            () => {{
                let id = file.id();

                drop(file);

                self.delete_file_by_id(id).await?;
            }};
        }

        let (created_at, file_name, file_type) = if file.is_new() {
            (file.created_at(), file.file_name().clone(), file.file_type().clone())
        } else {
            let created_at = Local::now();

            let file_type = if let Some((file_type, level)) = file_type {
                if matches!(level, FileTypeLevel::Manual | FileTypeLevel::ExactMatch) {
                    file_type
                } else {
                    // the fallback file type may not be correct, use the existing type instead
                    file.file_type().clone()
                }
            } else {
                file.file_type().clone()
            };

            let file_name = get_file_name(file_name, created_at, &file_type);

            (created_at, file_name, file_type)
        };

        let id = Uuid::new_v4();

        // insert resources
        {
            #[rustfmt::skip]
            let result = sqlx::query(
                "
                    INSERT INTO `resources` (`id`, `created_at`, `file_name`, `file_type`, `file_id`)
                        VALUES (?, ?, ?, ?, ?)
                ",
            )
            .bind(id)
            .bind(created_at.timestamp_millis())
            .bind(file_name.as_str())
            .bind(file_type.essence_str())
            .bind(file.id())
            .execute(&self.0.db)
            .await;

            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    recover_file!();

                    return Err(error.into());
                },
            };

            debug_assert!(result.rows_affected() > 0);
        }

        Ok(DatalithResource::new(id, created_at, file_type, file_name, file))
    }
}

// Download
impl Datalith {
    /// Check whether the resource exists or not.
    pub async fn check_resource_exist(
        &self,
        id: impl Into<Uuid>,
    ) -> Result<bool, DatalithReadError> {
        let current_timestamp = get_current_timestamp();

        #[rustfmt::skip]
        let row = sqlx::query(
            "
                SELECT
                    1
                FROM
                    `resources`
                JOIN `files` ON `files`.`id` = `resources`.`file_id`
                WHERE
                    `resources`.`id` = ?
                        AND ( `files`.`expired_at` IS NULL OR `files`.`expired_at` > ? )
            ",
        )
        .bind(id.into())
        .bind(current_timestamp)
        .fetch_optional(&self.0.db)
        .await?;

        Ok(row.is_some())
    }

    /// Retrieve the resource metadata using an ID.
    pub async fn get_resource_by_id(
        &self,
        id: impl Into<Uuid>,
    ) -> Result<Option<DatalithResource>, DatalithReadError> {
        let current_timestamp = get_current_timestamp();

        let id = id.into();

        #[rustfmt::skip]
        let row: Option<(i64, String, String, Uuid)> = sqlx::query_as(
            "
                SELECT
                    `resources`.`created_at`,
                    `resources`.`file_type`,
                    `resources`.`file_name`,
                    `resources`.`file_id`
                FROM
                    `resources`
                JOIN `files` ON `files`.`id` = `resources`.`file_id`
                WHERE
                    `resources`.`id` = ?
                        AND ( `files`.`expired_at` IS NULL OR `files`.`expired_at` > ? )
            ",
        )
        .bind(id)
        .bind(current_timestamp)
        .fetch_optional(&self.0.db)
        .await?;

        if let Some((created_at, file_type, file_name, file_id)) = row {
            let file = self.get_file_by_id(file_id).await?;

            if let Some(file) = file {
                let created_at = DateTime::from_timestamp_millis(created_at).unwrap();

                return Ok(Some(DatalithResource::new(
                    id,
                    created_at,
                    Mime::from_str(&file_type).unwrap(),
                    file_name,
                    file,
                )));
            }
        }

        Ok(None)
    }

    /// List resource IDs.
    pub async fn list_resource_ids(
        &self,
        mut pagination_options: PaginationOptions<DatalithResourceOrderBy>,
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
                                `resources`
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
                            `resources`
                        {sql_join}
                        {sql_order_by}
                        {sql_limit_offset}
                    "
                );

                let query = sqlx::query_as(&sql);

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
    /// Remove a resource using an ID. The related `DatalithResource` instances should be dropped before calling this function.
    #[inline]
    pub async fn delete_resource_by_id(
        &self,
        id: impl Into<Uuid>,
    ) -> Result<bool, DatalithReadError> {
        let id = id.into();

        #[rustfmt::skip]
        let row: Option<(Uuid,)> = sqlx::query_as(
            "
                SELECT
                    `file_id`
                FROM
                    `resources`
                WHERE
                    `id` = ?
            ",
        )
        .bind(id)
        .fetch_optional(&self.0.db)
        .await?;

        if let Some((file_id,)) = row {
            let guard = DeleteGuard::new(self.clone(), file_id).await;

            self.wait_for_opening_files(&guard).await?;

            #[rustfmt::skip]
            let result = sqlx::query(
                "
                    DELETE FROM
                        `resources`
                    WHERE
                        `id` = ?
                ",
            )
            .bind(id)
            .execute(&self.0.db)
            .await?;

            if result.rows_affected() == 0 {
                return Ok(false);
            }

            // delete the related file

            self.delete_file_by_id_inner(file_id, guard).await?;

            Ok(true)
        } else {
            Ok(false)
        }
    }
}
