use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use application::AppError;
use application::ports::repository::TrackRepository;
use domain::Track;

use crate::db::Database;

pub struct PgTrackRepository {
    db: Database,
}

impl PgTrackRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

#[async_trait]
impl TrackRepository for PgTrackRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Track>, AppError> {
        let row = sqlx::query_as::<_, Track>(
            r#"
            SELECT id, title, artist_display, album_id, track_number, disc_number,
                   duration_ms, genre, year, audio_fingerprint, file_modified_at,
                   file_size_bytes, blob_location, mbid, acoustid_id,
                   enrichment_status, enrichment_confidence, enrichment_attempts,
                   enrichment_locked, enriched_at, created_at, updated_at
            FROM tracks WHERE id = $1 LIMIT 1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.db.0)
        .await?;

        Ok(row)
    }

    async fn find_by_fingerprint(&self, fingerprint: &str) -> Result<Option<Track>, AppError> {
        let row = sqlx::query_as::<_, Track>(
            r#"
            SELECT id, title, artist_display, album_id, track_number, disc_number,
                   duration_ms, genre, year, audio_fingerprint, file_modified_at,
                   file_size_bytes, blob_location, mbid, acoustid_id,
                   enrichment_status, enrichment_confidence, enrichment_attempts,
                   enrichment_locked, enriched_at, created_at, updated_at
            FROM tracks WHERE audio_fingerprint = $1 LIMIT 1
            "#,
        )
        .bind(fingerprint)
        .fetch_optional(&self.db.0)
        .await?;

        Ok(row)
    }

    async fn find_by_blob_location(&self, location: &str) -> Result<Option<Track>, AppError> {
        let row = sqlx::query_as::<_, Track>(
            r#"
            SELECT id, title, artist_display, album_id, track_number, disc_number,
                   duration_ms, genre, year, audio_fingerprint, file_modified_at,
                   file_size_bytes, blob_location, mbid, acoustid_id,
                   enrichment_status, enrichment_confidence, enrichment_attempts,
                   enrichment_locked, enriched_at, created_at, updated_at
            FROM tracks WHERE blob_location = $1 LIMIT 1
            "#,
        )
        .bind(location)
        .fetch_optional(&self.db.0)
        .await?;

        Ok(row)
    }

    async fn find_many_by_blob_location(
        &self,
        locations: &[String],
    ) -> Result<std::collections::HashMap<String, Track>, AppError> {
        if locations.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let tracks = sqlx::query_as::<_, Track>(
            r#"
            SELECT id, title, artist_display, album_id, track_number, disc_number,
                   duration_ms, genre, year, audio_fingerprint, file_modified_at,
                   file_size_bytes, blob_location, mbid, acoustid_id,
                   enrichment_status, enrichment_confidence, enrichment_attempts,
                   enrichment_locked, enriched_at, created_at, updated_at
            FROM tracks WHERE blob_location = ANY($1)
            "#,
        )
        .bind(locations)
        .fetch_all(&self.db.0)
        .await?;

        let map: std::collections::HashMap<String, Track> = tracks
            .into_iter()
            .map(|t| (t.blob_location.clone(), t))
            .collect();

        Ok(map)
    }

    async fn insert(&self, track: &Track) -> Result<(Track, bool), AppError> {
        let mut tx = self.db.0.begin().await?;

        let result = sqlx::query(
            r#"
            INSERT INTO tracks (
                id, title, artist_display, album_id, track_number, disc_number,
                duration_ms, genre, year, audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                enrichment_status, enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                $14, $15, $16, $17, $18, $19, $20, $21, $22
            )
            ON CONFLICT (audio_fingerprint) DO NOTHING
            "#,
        )
        .bind(track.id)
        .bind(&track.title)
        .bind(&track.artist_display)
        .bind(track.album_id)
        .bind(track.track_number)
        .bind(track.disc_number)
        .bind(track.duration_ms)
        .bind(&track.genre)
        .bind(track.year)
        .bind(&track.audio_fingerprint)
        .bind(track.file_modified_at)
        .bind(track.file_size_bytes)
        .bind(&track.blob_location)
        .bind(&track.mbid)
        .bind(&track.acoustid_id)
        .bind(&track.enrichment_status)
        .bind(track.enrichment_confidence)
        .bind(track.enrichment_attempts)
        .bind(track.enrichment_locked)
        .bind(track.enriched_at)
        .bind(track.created_at)
        .bind(track.updated_at)
        .execute(&mut *tx)
        .await?;

        let is_inserted = result.rows_affected() > 0;

        if !is_inserted {
            sqlx::query(
                r#"
                UPDATE tracks
                SET blob_location = $1, file_modified_at = $2, file_size_bytes = $3, updated_at = now()
                WHERE audio_fingerprint = $4
                "#
            )
            .bind(&track.blob_location)
            .bind(track.file_modified_at)
            .bind(track.file_size_bytes)
            .bind(&track.audio_fingerprint)
            .execute(&mut *tx)
            .await?;
        }

        let returned_track = sqlx::query_as::<_, Track>(
            r#"
            SELECT id, title, artist_display, album_id, track_number, disc_number,
                   duration_ms, genre, year, audio_fingerprint, file_modified_at,
                   file_size_bytes, blob_location, mbid, acoustid_id,
                   enrichment_status, enrichment_confidence, enrichment_attempts,
                   enrichment_locked, enriched_at, created_at, updated_at
            FROM tracks
            WHERE audio_fingerprint = $1
            "#,
        )
        .bind(&track.audio_fingerprint)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok((returned_track, is_inserted))
    }

    async fn update_file_identity(
        &self,
        id: Uuid,
        file_modified_at: DateTime<Utc>,
        file_size_bytes: i64,
        blob_location: &str,
    ) -> Result<(), AppError> {
        sqlx::query(
            r#"
            UPDATE tracks
            SET file_modified_at = $2,
                file_size_bytes  = $3,
                blob_location    = $4,
                updated_at       = now()
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(file_modified_at)
        .bind(file_size_bytes)
        .bind(blob_location)
        .execute(&self.db.0)
        .await?;

        Ok(())
    }

    async fn update_fingerprint(&self, id: Uuid, fingerprint: &str) -> Result<(), AppError> {
        sqlx::query("UPDATE tracks SET audio_fingerprint = $1, updated_at = now() WHERE id = $2")
            .bind(fingerprint)
            .bind(id)
            .execute(&self.db.0)
            .await?;

        Ok(())
    }

    async fn update_enrichment_status(
        &self,
        id: Uuid,
        status: &domain::EnrichmentStatus,
        attempts: i32,
        enriched_at: Option<DateTime<Utc>>,
    ) -> Result<(), AppError> {
        sqlx::query(
            r#"
            UPDATE tracks
            SET enrichment_status  = $2,
                enrichment_attempts = $3,
                enriched_at         = $4,
                updated_at          = now()
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(status)
        .bind(attempts)
        .bind(enriched_at)
        .execute(&self.db.0)
        .await?;

        Ok(())
    }

    async fn update_enriched_metadata(
        &self,
        _id: Uuid,
        _title: &str,
        _artist_display: &str,
        _album_id: Option<Uuid>,
        _genre: Option<&str>,
        _year: Option<i32>,
        _mbid: Option<&str>,
        _acoustid_id: Option<&str>,
        _confidence: Option<f32>,
    ) -> Result<(), AppError> {
        todo!("update_enriched_metadata — implemented in Pass 3")
    }

    async fn claim_for_enrichment(
        &self,
        failed_retry_limit: u32,
        unmatched_retry_limit: u32,
        limit: i64,
    ) -> Result<Vec<Track>, AppError> {
        let mut tx = self.db.0.begin().await?;

        // SELECT candidates with FOR UPDATE SKIP LOCKED
        let tracks = sqlx::query_as::<_, Track>(
            r#"
            SELECT id, title, artist_display, album_id, track_number, disc_number,
                   duration_ms, genre, year, audio_fingerprint, file_modified_at,
                   file_size_bytes, blob_location, mbid, acoustid_id,
                   enrichment_status, enrichment_confidence, enrichment_attempts,
                   enrichment_locked, enriched_at, created_at, updated_at
            FROM tracks
            WHERE enrichment_locked = false
              AND (
                (enrichment_status IN ('pending', 'failed', 'low_confidence')
                 AND enrichment_attempts < $1
                 AND (enriched_at IS NULL OR enriched_at < now() - INTERVAL '1 hour'))
                OR
                (enrichment_status = 'unmatched'
                 AND enrichment_attempts < $2
                 AND enriched_at < now() - INTERVAL '24 hours')
              )
            ORDER BY created_at ASC
            LIMIT $3
            FOR UPDATE SKIP LOCKED
            "#,
        )
        .bind(failed_retry_limit as i32)
        .bind(unmatched_retry_limit as i32)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;

        if !tracks.is_empty() {
            let ids: Vec<Uuid> = tracks.iter().map(|t| t.id).collect();
            sqlx::query(
                r#"
                UPDATE tracks
                SET enrichment_status = 'enriching', updated_at = now()
                WHERE id = ANY($1)
                "#,
            )
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(tracks)
    }

    async fn reset_stale_enriching(&self) -> Result<u64, AppError> {
        let result = sqlx::query(
            r#"
            UPDATE tracks
            SET enrichment_status = 'pending',
                enrichment_locked = false,
                updated_at = now()
            WHERE enrichment_status = 'enriching'
            "#,
        )
        .execute(&self.db.0)
        .await?;

        Ok(result.rows_affected())
    }

    async fn force_rescan(&self) -> Result<u64, AppError> {
        let result = sqlx::query(
            r#"
            UPDATE tracks
            SET enrichment_status = 'pending',
                enrichment_attempts = 0,
                enrichment_locked = false,
                updated_at = now()
            WHERE enrichment_status IN ('exhausted', 'low_confidence')
            "#,
        )
        .execute(&self.db.0)
        .await?;

        Ok(result.rows_affected())
    }

    async fn mark_file_missing(&self, blob_location: &str) -> Result<(), AppError> {
        sqlx::query(
            r#"
            UPDATE tracks
            SET enrichment_status = 'file_missing',
                enrichment_locked = false,
                updated_at = now()
            WHERE blob_location = $1
            "#,
        )
        .bind(blob_location)
        .execute(&self.db.0)
        .await?;

        Ok(())
    }
}
