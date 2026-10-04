use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use super::{
    DatalithService, Media, MediaKind, PlaybackSession, ServiceError, migration::timestamp,
};

impl DatalithService {
    /// Claim the fixed playback window of single-use audio or video.
    /// Repeating an active claim with the same idempotency key returns the same credential.
    pub async fn claim_playback_session(
        &self,
        id: Uuid,
        key: Option<String>,
    ) -> Result<PlaybackSession, ServiceError> {
        super::tasks::validate_idempotency_key(key.as_deref())?;
        if let Some(key) = &key
            && let Some(session) = self.existing_claim(id, key).await?
        {
            return Ok(session);
        }
        let _gate = self.0.writes.try_read().map_err(|_| ServiceError::Busy)?;
        let _mutation = self.0.mutations.lock().await;
        if let Some(key) = &key
            && let Some(session) = self.existing_claim(id, key).await?
        {
            return Ok(session);
        }
        let mut tx = self.0.datalith.0.db.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "SELECT metadata FROM media WHERE id=? AND consumed_at IS NULL AND (expires_at IS \
             NULL OR expires_at>?)",
        )
        .bind(id)
        .bind(Utc::now().timestamp_millis())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ServiceError::NotFound)?;
        let media: Media = serde_json::from_str(row.try_get("metadata")?)?;
        if !media.single_use || !matches!(media.kind, MediaKind::Audio | MediaKind::Video) {
            return Err(ServiceError::Conflict(
                "playback sessions require single-use audio or video".into(),
            ));
        }
        let expiry = deadline(self.0.config.playback_session_seconds)?;
        let expires_at = media.expires_at.map_or(expiry, |media_expiry| media_expiry.min(expiry));
        let expires_at = timestamp(expires_at.timestamp_millis())?;
        let token = hex::encode(crate::functions::get_random_hash());
        sqlx::query(
            "INSERT INTO playback_sessions(media_id, token, token_hash, expires_at, \
             idempotency_key) VALUES(?,?,?,?,?)",
        )
        .bind(id)
        .bind(&token)
        .bind(token_hash(&token)?)
        .bind(expires_at.timestamp_millis())
        .bind(key)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE media SET consumed_at=? WHERE id=?")
            .bind(Utc::now().timestamp_millis())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(PlaybackSession {
            token,
            expires_at,
        })
    }

    async fn existing_claim(
        &self,
        id: Uuid,
        key: &str,
    ) -> Result<Option<PlaybackSession>, ServiceError> {
        let row = sqlx::query(
            "SELECT media_id, token, expires_at FROM playback_sessions WHERE idempotency_key=?",
        )
        .bind(key)
        .fetch_optional(&self.0.datalith.0.db)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        if row.try_get::<Uuid, _>("media_id")? != id {
            return Err(ServiceError::Conflict(
                "Idempotency-Key was already used for a different playback claim".into(),
            ));
        }
        let token: String = row.try_get("token")?;
        self.authorize_media(id, Some(&token)).await?;
        Ok(Some(PlaybackSession {
            token,
            expires_at: timestamp(row.try_get("expires_at")?)?,
        }))
    }

    pub(super) async fn authorize_media(
        &self,
        id: Uuid,
        token: Option<&str>,
    ) -> Result<Media, ServiceError> {
        let hash = token.map(token_hash).transpose()?;
        self.authorize_media_hash(id, hash.as_deref()).await
    }

    pub(super) async fn authorize_media_hash(
        &self,
        id: Uuid,
        hash: Option<&str>,
    ) -> Result<Media, ServiceError> {
        let now = Utc::now().timestamp_millis();
        let row = sqlx::query(
            "SELECT metadata, consumed_at FROM media WHERE id=? AND (expires_at IS NULL OR \
             expires_at>?)",
        )
        .bind(id)
        .bind(now)
        .fetch_optional(&self.0.datalith.0.db)
        .await?
        .ok_or(ServiceError::NotFound)?;
        let mut media: Media = serde_json::from_str(row.try_get("metadata")?)?;
        media.consumed_at =
            row.try_get::<Option<i64>, _>("consumed_at")?.map(timestamp).transpose()?;
        if media.single_use && matches!(media.kind, MediaKind::Audio | MediaKind::Video) {
            let hash = hash.ok_or(ServiceError::NotFound)?;
            let valid = sqlx::query(
                "SELECT 1 FROM playback_sessions WHERE media_id=? AND token_hash=? AND \
                 expires_at>?",
            )
            .bind(id)
            .bind(hash)
            .bind(now)
            .fetch_optional(&self.0.datalith.0.db)
            .await?
            .is_some();
            if !valid {
                return Err(ServiceError::NotFound);
            }
        } else if media.consumed_at.is_some() {
            return Err(ServiceError::NotFound);
        }
        Ok(media)
    }
}

pub(super) fn token_hash(token: &str) -> Result<String, ServiceError> {
    if token.len() != 64
        || !token.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(ServiceError::NotFound);
    }
    Ok(hex::encode(Sha256::digest(token.as_bytes())))
}

pub(super) fn deadline(seconds: u64) -> Result<DateTime<Utc>, ServiceError> {
    i64::try_from(seconds)
        .ok()
        .and_then(chrono::Duration::try_seconds)
        .and_then(|duration| Utc::now().checked_add_signed(duration))
        .ok_or_else(|| ServiceError::Invalid("retention exceeds the supported date range".into()))
}
