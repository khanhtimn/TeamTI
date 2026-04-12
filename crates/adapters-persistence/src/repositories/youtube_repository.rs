use async_trait::async_trait;
use uuid::Uuid;

use application::AppError;
use application::ports::youtube::YoutubeRepository;
use domain::{EnrichmentStatus, Track, YoutubeDownloadJob};

use crate::db::Database;

pub struct PgYoutubeRepository {
    db: Database,
}

impl PgYoutubeRepository {
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

#[async_trait]
impl YoutubeRepository for PgYoutubeRepository {
    async fn create_youtube_stub(&self, meta: &domain::VideoMetadata) -> Result<Uuid, AppError> {
        let id = Uuid::new_v4();

        // Use the yt-dlp "track" field if available (music video metadata),
        // otherwise fall back to the video title.
        let title = meta
            .track_title
            .as_deref()
            .unwrap_or_else(|| meta.title.as_deref().unwrap_or("Unknown Title"));
        let artist_display = meta
            .artist
            .as_deref()
            .unwrap_or_else(|| meta.uploader.as_deref().unwrap_or("Unknown Uploader"));

        let result = sqlx::query!(
            r#"
            INSERT INTO tracks (
                id, title, artist_display, duration_ms,
                source, youtube_video_id, youtube_channel_id,
                youtube_uploader, youtube_thumbnail_url,
                enrichment_status, created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, 'youtube', $5, $6, $7, $8, 'pending', now(), now()
            )
            ON CONFLICT (youtube_video_id) WHERE youtube_video_id IS NOT NULL
            DO NOTHING
            "#,
            id,
            title,
            artist_display,
            meta.duration_ms,
            meta.video_id,
            meta.channel_id,
            meta.uploader,
            meta.thumbnail_url,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("youtube.create_stub"))?;

        if result.rows_affected() == 0 {
            // ON CONFLICT: row already existed — look it up
            let existing = sqlx::query_scalar!(
                r#"SELECT id FROM tracks WHERE youtube_video_id = $1"#,
                meta.video_id,
            )
            .fetch_one(&self.db.0)
            .await
            .map_err(crate::db_err!("youtube.create_stub_lookup"))?;

            return Ok(existing);
        }

        Ok(id)
    }

    async fn create_youtube_stubs_batch(
        &self,
        metas: &[domain::VideoMetadata],
    ) -> Result<Vec<(String, Uuid)>, AppError> {
        if metas.is_empty() {
            return Ok(Vec::new());
        }

        let mut ids = Vec::with_capacity(metas.len());
        let mut titles = Vec::with_capacity(metas.len());
        let mut artist_displays = Vec::with_capacity(metas.len());
        let mut duration_ms = Vec::with_capacity(metas.len());
        let mut youtube_video_ids = Vec::with_capacity(metas.len());
        let mut youtube_channel_ids = Vec::with_capacity(metas.len());
        let mut youtube_uploaders = Vec::with_capacity(metas.len());
        let mut youtube_thumbnail_urls = Vec::with_capacity(metas.len());

        for meta in metas {
            ids.push(Uuid::new_v4());
            titles.push(
                meta.track_title
                    .as_deref()
                    .unwrap_or_else(|| meta.title.as_deref().unwrap_or("Unknown Title"))
                    .to_string(),
            );
            artist_displays.push(
                meta.artist
                    .as_deref()
                    .unwrap_or_else(|| meta.uploader.as_deref().unwrap_or("Unknown Uploader"))
                    .to_string(),
            );
            duration_ms.push(meta.duration_ms);
            youtube_video_ids.push(meta.video_id.clone());
            youtube_channel_ids.push(meta.channel_id.clone());
            youtube_uploaders.push(meta.uploader.clone());
            youtube_thumbnail_urls.push(meta.thumbnail_url.clone());
        }

        let rows = sqlx::query!(
            r#"
            WITH input_data AS (
                SELECT * FROM unnest(
                    $1::uuid[],
                    $2::text[],
                    $3::text[],
                    $4::bigint[],
                    $5::text[],
                    $6::text[],
                    $7::text[],
                    $8::text[]
                ) AS t(id, title, artist_display, duration_ms, video_id, channel_id, uploader, thumbnail)
            ),
            inserted AS (
                INSERT INTO tracks (
                    id, title, artist_display, duration_ms,
                    source, youtube_video_id, youtube_channel_id,
                    youtube_uploader, youtube_thumbnail_url,
                    enrichment_status, created_at, updated_at
                )
                SELECT
                    id, title, artist_display, duration_ms,
                    'youtube', video_id, channel_id,
                    uploader, thumbnail,
                    'pending', now(), now()
                FROM input_data
                ON CONFLICT (youtube_video_id) WHERE youtube_video_id IS NOT NULL
                DO NOTHING
                RETURNING youtube_video_id, id
            )
            SELECT i.youtube_video_id as video_id, i.id
            FROM inserted i
            UNION ALL
            SELECT t.youtube_video_id as video_id, t.id
            FROM tracks t
            JOIN input_data inp ON t.youtube_video_id = inp.video_id
            "#,
            &ids,
            &titles,
            &artist_displays,
            &duration_ms as &[Option<i64>],
            &youtube_video_ids,
            &youtube_channel_ids as &[Option<String>],
            &youtube_uploaders as &[Option<String>],
            &youtube_thumbnail_urls as &[Option<String>]
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(crate::db_err!("youtube.create_stubs_batch"))?;

        Ok(rows
            .into_iter()
            .filter_map(|r| r.video_id.zip(r.id))
            .collect())
    }

    async fn find_track_by_video_id(&self, video_id: &str) -> Result<Option<Track>, AppError> {
        let row = sqlx::query_as!(
            Track,
            r#"SELECT
                id, title, artist_display, album_id, track_number, disc_number,
                duration_ms, genres, year, bpm, isrc, lyrics, bitrate, sample_rate, channels, codec,
                audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                source, youtube_video_id, youtube_channel_id, youtube_uploader, youtube_thumbnail_url,
                enrichment_status as "enrichment_status: EnrichmentStatus",
                enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at, tags_written_at,
                analysis_status as "analysis_status: domain::AnalysisStatus",
                analysis_attempts, analysis_locked, analyzed_at
            FROM tracks WHERE youtube_video_id = $1 LIMIT 1"#,
            video_id,
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(crate::db_err!("youtube.find_by_video_id"))?;

        Ok(row)
    }

    async fn update_youtube_stub_metadata(
        &self,
        video_id: &str,
        meta: &domain::VideoMetadata,
    ) -> Result<(), AppError> {
        let title = meta
            .track_title
            .as_deref()
            .unwrap_or_else(|| meta.title.as_deref().unwrap_or("Unknown Title"));
        let artist_display = meta
            .artist
            .as_deref()
            .unwrap_or_else(|| meta.uploader.as_deref().unwrap_or("Unknown Uploader"));

        sqlx::query!(
            r#"
            UPDATE tracks 
            SET title = $1,
                artist_display = $2,
                duration_ms = $3,
                youtube_channel_id = $4,
                youtube_uploader = $5,
                youtube_thumbnail_url = $6,
                updated_at = now()
            WHERE youtube_video_id = $7
            "#,
            title,
            artist_display,
            meta.duration_ms,
            meta.channel_id,
            meta.uploader,
            meta.thumbnail_url,
            video_id
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("youtube.update_stub"))?;

        Ok(())
    }

    async fn get_download_job(
        &self,
        video_id: &str,
    ) -> Result<Option<YoutubeDownloadJob>, AppError> {
        let row = sqlx::query_as!(
            YoutubeDownloadJob,
            r#"SELECT
                video_id, track_id, url, status, attempts, error_message,
                created_at, started_at, completed_at
            FROM youtube_download_jobs WHERE video_id = $1"#,
            video_id,
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(crate::db_err!("youtube.get_download_job"))?;

        Ok(row)
    }

    async fn upsert_download_job(
        &self,
        job: &domain::NewYoutubeDownloadJob,
    ) -> Result<(), AppError> {
        sqlx::query!(
            r#"
            INSERT INTO youtube_download_jobs (video_id, track_id, url)
            VALUES ($1, $2, $3)
            ON CONFLICT (video_id) DO NOTHING
            "#,
            job.video_id,
            job.track_id,
            job.url,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("youtube.upsert_download_job"))?;

        Ok(())
    }

    async fn lock_download_job(&self, video_id: &str) -> Result<bool, AppError> {
        let result = sqlx::query!(
            r#"
            UPDATE youtube_download_jobs
            SET status = 'downloading', started_at = now()
            WHERE video_id = $1 AND status IN ('pending', 'failed')
            "#,
            video_id,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("youtube.lock_job"))?;

        Ok(result.rows_affected() > 0)
    }

    async fn complete_download_job(
        &self,
        video_id: &str,
        blob_location: &str,
    ) -> Result<(), AppError> {
        let mut tx = self
            .db
            .0
            .begin()
            .await
            .map_err(crate::db_err!("youtube.complete_job"))?;

        sqlx::query!(
            r#"
            UPDATE youtube_download_jobs
            SET status = 'done', completed_at = now()
            WHERE video_id = $1
            "#,
            video_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(crate::db_err!("youtube.complete_job"))?;

        sqlx::query!(
            r#"
            UPDATE tracks
            SET blob_location = $2, updated_at = now()
            WHERE youtube_video_id = $1
            "#,
            video_id,
            blob_location,
        )
        .execute(&mut *tx)
        .await
        .map_err(crate::db_err!("youtube.complete_job"))?;

        tx.commit()
            .await
            .map_err(crate::db_err!("youtube.complete_job"))?;

        Ok(())
    }

    async fn fail_download_job(&self, video_id: &str, error: &str) -> Result<(), AppError> {
        sqlx::query!(
            r#"
            UPDATE youtube_download_jobs
            SET status = 'failed',
                attempts = attempts + 1,
                error_message = $2
            WHERE video_id = $1
            "#,
            video_id,
            error,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("youtube.fail_job"))?;

        Ok(())
    }

    async fn permanently_fail_download_job(&self, video_id: &str) -> Result<(), AppError> {
        let mut tx = self
            .db
            .0
            .begin()
            .await
            .map_err(crate::db_err!("youtube.perm_fail_job"))?;

        // Mark the job permanently failed
        sqlx::query!(
            r#"
            UPDATE youtube_download_jobs
            SET status = 'permanently_failed',
                attempts = attempts + 1
            WHERE video_id = $1
            "#,
            video_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(crate::db_err!("youtube.perm_fail_job"))?;

        // Conditionally delete the stub — only if it has zero listen events.
        // If it has listen history, keep it for user visibility.
        sqlx::query!(
            r#"
            DELETE FROM tracks
            WHERE youtube_video_id = $1
              AND NOT EXISTS (
                  SELECT 1 FROM listen_events WHERE track_id = tracks.id
              )
            "#,
            video_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(crate::db_err!("youtube.perm_fail_job"))?;

        tx.commit()
            .await
            .map_err(crate::db_err!("youtube.perm_fail_job"))?;

        Ok(())
    }

    async fn unlock_stale_download_jobs(
        &self,
        older_than: std::time::Duration,
    ) -> Result<u64, AppError> {
        let result = sqlx::query!(
            r#"
            UPDATE youtube_download_jobs
            SET status = 'pending', started_at = NULL
            WHERE status = 'downloading'
              AND started_at < NOW() - make_interval(secs => $1)
            "#,
            older_than.as_secs() as f64,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("youtube.unlock_stale"))?;

        Ok(result.rows_affected())
    }
}
