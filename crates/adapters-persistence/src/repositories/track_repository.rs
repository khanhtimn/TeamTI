use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use application::AppError;
use application::ports::repository::TrackRepository;
use domain::{EnrichmentStatus, Track};

use crate::db::Database;

pub struct PgTrackRepository {
    db: Database,
}

impl PgTrackRepository {
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

#[async_trait]
impl TrackRepository for PgTrackRepository {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Track>, AppError> {
        let row = sqlx::query_as!(
            Track,
            r#"SELECT
                id, title, artist_display, album_id, track_number, disc_number,
                duration_ms, genres, year, bpm, isrc, lyrics, bitrate, sample_rate, channels, codec,
                audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                enrichment_status as "enrichment_status: EnrichmentStatus",
                enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at, tags_written_at
            FROM tracks WHERE id = $1 LIMIT 1"#,
            id,
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(crate::db_err!("track.find_by_id"))?;

        Ok(row)
    }

    async fn find_by_fingerprint(&self, fingerprint: &str) -> Result<Option<Track>, AppError> {
        let row = sqlx::query_as!(
            Track,
            r#"SELECT
                id, title, artist_display, album_id, track_number, disc_number,
                duration_ms, genres, year, bpm, isrc, lyrics, bitrate, sample_rate, channels, codec,
                audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                enrichment_status as "enrichment_status: EnrichmentStatus",
                enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at, tags_written_at
            FROM tracks WHERE fingerprint_hash = md5($1) LIMIT 1"#,
            fingerprint,
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(crate::db_err!("track.find_by_fingerprint"))?;

        Ok(row)
    }

    async fn find_by_blob_location(&self, location: &str) -> Result<Option<Track>, AppError> {
        let row = sqlx::query_as!(
            Track,
            r#"SELECT
                id, title, artist_display, album_id, track_number, disc_number,
                duration_ms, genres, year, bpm, isrc, lyrics, bitrate, sample_rate, channels, codec,
                audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                enrichment_status as "enrichment_status: EnrichmentStatus",
                enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at, tags_written_at
            FROM tracks WHERE blob_location = $1 LIMIT 1"#,
            location,
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(crate::db_err!("track.find_by_blob_location"))?;

        Ok(row)
    }

    async fn find_many_by_blob_location(
        &self,
        locations: &[String],
    ) -> Result<std::collections::HashMap<String, Track>, AppError> {
        if locations.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        // PERF-2: Use UNNEST + JOIN instead of = ANY($1) for better index
        // utilization on large arrays. PostgreSQL can plan this as an
        // index nested-loop join regardless of array size.
        //
        // Category 3: Dynamic query — UNNEST($1::text[]) is not supported
        // by query_as! due to the explicit cast. Stays as runtime query_as.
        let q = r"
            SELECT
                id, title, artist_display, album_id, track_number, disc_number,
                duration_ms, genres, year, bpm, isrc, lyrics, bitrate, sample_rate, channels, codec,
                audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                enrichment_status, enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at, tags_written_at
            FROM tracks t
            JOIN UNNEST($1::text[]) AS u(loc) ON t.blob_location = u.loc
        ";
        let tracks = sqlx::query_as::<_, Track>(q)
            .bind(locations)
            .fetch_all(&self.db.0)
            .await
            .map_err(crate::db_err!("track.find_many_by_blob_location"))?;

        let map: std::collections::HashMap<String, Track> = tracks
            .into_iter()
            .map(|t| (t.blob_location.clone(), t))
            .collect();

        Ok(map)
    }

    async fn insert(&self, track: &Track) -> Result<(Track, bool), AppError> {
        let mut tx = self
            .db
            .0
            .begin()
            .await
            .map_err(crate::db_err!("track.insert"))?;

        let result = sqlx::query!(
            r#"
            INSERT INTO tracks (
                id, title, artist_display, album_id, track_number, disc_number,
                duration_ms, genres, year, bpm, isrc, lyrics, bitrate, sample_rate, channels, codec,
                audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                enrichment_status, enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24,
                $25, $26, $27, $28, $29
            )
            ON CONFLICT (fingerprint_hash) DO NOTHING
            "#,
            track.id,
            track.title,
            track.artist_display,
            track.album_id,
            track.track_number,
            track.disc_number,
            track.duration_ms,
            track.genres.as_deref() as Option<&[String]>,
            track.year,
            track.bpm,
            track.isrc,
            track.lyrics,
            track.bitrate,
            track.sample_rate,
            track.channels,
            track.codec,
            track.audio_fingerprint,
            track.file_modified_at,
            track.file_size_bytes,
            track.blob_location,
            track.mbid,
            track.acoustid_id,
            &track.enrichment_status as &EnrichmentStatus,
            track.enrichment_confidence,
            track.enrichment_attempts,
            track.enrichment_locked,
            track.enriched_at,
            track.created_at,
            track.updated_at,
        )
        .execute(&mut *tx)
        .await
        .map_err(crate::db_err!("track.insert"))?;

        let is_inserted = result.rows_affected() > 0;

        if !is_inserted {
            sqlx::query!(
                r#"
                UPDATE tracks
                SET blob_location = $1, file_modified_at = $2, file_size_bytes = $3, updated_at = now()
                WHERE fingerprint_hash = md5($4)
                "#,
                track.blob_location,
                track.file_modified_at,
                track.file_size_bytes,
                track.audio_fingerprint,
            )
            .execute(&mut *tx)
            .await
            .map_err(crate::db_err!("track.insert"))?;
        }

        let returned_track = sqlx::query_as!(
            Track,
            r#"SELECT
                id, title, artist_display, album_id, track_number, disc_number,
                duration_ms, genres, year, bpm, isrc, lyrics, bitrate, sample_rate, channels, codec,
                audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                enrichment_status as "enrichment_status: EnrichmentStatus",
                enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at, tags_written_at
            FROM tracks WHERE fingerprint_hash = md5($1)"#,
            track.audio_fingerprint,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(crate::db_err!("track.insert"))?;

        tx.commit().await.map_err(crate::db_err!("track.insert"))?;

        Ok((returned_track, is_inserted))
    }

    async fn update_file_identity(
        &self,
        id: Uuid,
        file_modified_at: DateTime<Utc>,
        file_size_bytes: i64,
        blob_location: &str,
    ) -> Result<(), AppError> {
        sqlx::query!(
            r#"
            UPDATE tracks
            SET file_modified_at = $2,
                file_size_bytes  = $3,
                blob_location    = $4,
                updated_at       = now()
            WHERE id = $1
            "#,
            id,
            file_modified_at,
            file_size_bytes,
            blob_location,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("track.update_file_identity"))?;

        Ok(())
    }

    async fn update_fingerprint(&self, id: Uuid, fingerprint: &str) -> Result<(), AppError> {
        sqlx::query!(
            "UPDATE tracks SET audio_fingerprint = $1, updated_at = now() WHERE id = $2",
            fingerprint,
            id,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("track.update_fingerprint"))?;

        Ok(())
    }

    async fn update_acoustid_match(
        &self,
        id: Uuid,
        acoustid_id: &str,
        confidence: f32,
    ) -> Result<(), AppError> {
        sqlx::query!(
            r#"
            UPDATE tracks
            SET acoustid_id           = $2,
                enrichment_confidence = $3,
                updated_at            = now()
            WHERE id = $1
            "#,
            id,
            acoustid_id,
            confidence,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("track.update_acoustid_match"))?;

        Ok(())
    }

    async fn update_enrichment_status(
        &self,
        id: Uuid,
        status: &domain::EnrichmentStatus,
        attempts: i32,
        enriched_at: Option<DateTime<Utc>>,
    ) -> Result<(), AppError> {
        sqlx::query!(
            r#"
            UPDATE tracks
            SET enrichment_status  = $2,
                enrichment_attempts = $3,
                enriched_at         = $4,
                updated_at          = now()
            WHERE id = $1
            "#,
            id,
            status as &EnrichmentStatus,
            attempts,
            enriched_at,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("track.update_enrichment_status"))?;

        Ok(())
    }

    async fn update_enriched_metadata(
        &self,
        id: Uuid,
        title: &str,
        artist_display: &str,
        album_id: Option<Uuid>,
        genres: Option<Vec<String>>,
        year: Option<i32>,
        mbid: Option<&str>,
        acoustid_id: Option<&str>,
        confidence: Option<f32>,
        isrc: Option<&str>,
    ) -> Result<(), AppError> {
        sqlx::query!(
            r#"
            UPDATE tracks
            SET title = COALESCE($2, title),
                artist_display = COALESCE($3, artist_display),
                album_id = COALESCE($4, album_id),
                genres = COALESCE($5, genres),
                year = COALESCE($6, year),
                mbid = COALESCE($7, mbid),
                acoustid_id = COALESCE($8, acoustid_id),
                enrichment_confidence = COALESCE($9, enrichment_confidence),
                isrc = COALESCE($10, isrc),
                enrichment_status = 'done',
                enrichment_locked = false,
                enriched_at = now(),
                updated_at = now()
            WHERE id = $1
            "#,
            id,
            title,
            artist_display,
            album_id,
            genres.as_deref() as Option<&[String]>,
            year,
            mbid,
            acoustid_id,
            confidence,
            isrc,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("track.update_enriched_metadata"))?;
        Ok(())
    }

    /// CRIT-3: Atomically claim a single track for the reactive enrichment path.
    /// Returns the track with status already set to 'enriching'.
    /// Returns None if the track doesn't exist or isn't in a claimable state
    /// (e.g., already claimed by the proactive poll path).
    async fn claim_single(&self, id: Uuid) -> Result<Option<Track>, AppError> {
        let row = sqlx::query_as!(
            Track,
            r#"UPDATE tracks
            SET enrichment_status = 'enriching', updated_at = now()
            WHERE id = $1 AND enrichment_status = 'pending'
            RETURNING
                id, title, artist_display, album_id, track_number, disc_number,
                duration_ms, genres, year, bpm, isrc, lyrics, bitrate, sample_rate, channels, codec,
                audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                enrichment_status as "enrichment_status: EnrichmentStatus",
                enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at, tags_written_at"#,
            id,
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(crate::db_err!("track.claim_single"))?;

        Ok(row)
    }

    /// DESIGN-5 fix: Uses UPDATE ... RETURNING to return tracks
    /// with the post-update state (`enrichment_status` = 'enriching'),
    /// preventing downstream footguns from stale status values.
    async fn claim_for_enrichment(
        &self,
        failed_retry_limit: i32,
        unmatched_retry_limit: i32,
        limit: i64,
    ) -> Result<Vec<Track>, AppError> {
        let tracks = sqlx::query_as!(
            Track,
            r#"UPDATE tracks
            SET enrichment_status = 'enriching', updated_at = now()
            WHERE id IN (
                SELECT id FROM tracks
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
            )
            RETURNING
                id, title, artist_display, album_id, track_number, disc_number,
                duration_ms, genres, year, bpm, isrc, lyrics, bitrate, sample_rate, channels, codec,
                audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                enrichment_status as "enrichment_status: EnrichmentStatus",
                enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at, tags_written_at"#,
            failed_retry_limit,
            unmatched_retry_limit,
            limit,
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(crate::db_err!("track.claim_for_enrichment"))?;

        Ok(tracks)
    }

    async fn reset_stale_enriching(&self) -> Result<u64, AppError> {
        let result = sqlx::query!(
            r#"
            UPDATE tracks
            SET enrichment_status = 'pending',
                enrichment_locked = false,
                updated_at = now()
            WHERE enrichment_status = 'enriching'
            "#,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("track.reset_stale_enriching"))?;

        Ok(result.rows_affected())
    }

    async fn force_rescan(&self) -> Result<u64, AppError> {
        let result = sqlx::query!(
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
        .await
        .map_err(crate::db_err!("track.force_rescan"))?;

        Ok(result.rows_affected())
    }

    async fn mark_file_missing(&self, blob_location: &str) -> Result<(), AppError> {
        sqlx::query!(
            r#"
            UPDATE tracks
            SET enrichment_status = 'file_missing',
                enrichment_locked = false,
                updated_at = now()
            WHERE blob_location = $1
            "#,
            blob_location,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("track.mark_file_missing"))?;

        Ok(())
    }

    /// A4 fix: includes AND `enrichment_status` = 'done' safety guard.
    /// If a non-done track ID somehow reaches the Tag Writer channel,
    /// this is a no-op instead of permanently suppressing future writeback.
    async fn update_file_tags_written(
        &self,
        id: Uuid,
        new_mtime: DateTime<Utc>,
        new_size_bytes: i64,
    ) -> Result<(), AppError> {
        let result = sqlx::query!(
            r#"
            UPDATE tracks
            SET file_modified_at = $2,
                file_size_bytes  = $3,
                tags_written_at  = now(),
                updated_at       = now()
            WHERE id = $1
              AND enrichment_status = 'done'
            "#,
            id,
            new_mtime,
            new_size_bytes,
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("track.update_file_tags_written"))?;

        if result.rows_affected() == 0 {
            tracing::debug!(
                "update_file_tags_written: no rows affected for {} (not done?)",
                id
            );
        }

        Ok(())
    }

    async fn find_tags_unwritten(&self, limit: i64) -> Result<Vec<Track>, AppError> {
        let tracks = sqlx::query_as!(
            Track,
            r#"SELECT
                id, title, artist_display, album_id, track_number, disc_number,
                duration_ms, genres, year, bpm, isrc, lyrics, bitrate, sample_rate, channels, codec,
                audio_fingerprint, file_modified_at,
                file_size_bytes, blob_location, mbid, acoustid_id,
                enrichment_status as "enrichment_status: EnrichmentStatus",
                enrichment_confidence, enrichment_attempts,
                enrichment_locked, enriched_at, created_at, updated_at, tags_written_at
            FROM tracks
            WHERE enrichment_status = 'done' AND tags_written_at IS NULL
            ORDER BY updated_at ASC LIMIT $1"#,
            limit,
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(crate::db_err!("track.find_tags_unwritten"))?;

        Ok(tracks)
    }

    async fn update_lyrics(&self, track_id: Uuid, lyrics: &str) -> Result<(), AppError> {
        sqlx::query!(
            r#"
            UPDATE tracks
            SET lyrics = $1, updated_at = NOW()
            WHERE id = $2
            "#,
            lyrics,
            track_id
        )
        .execute(&self.db.0)
        .await
        .map_err(crate::db_err!("track.update_lyrics"))?;

        Ok(())
    }

    async fn get_credits(
        &self,
        track_id: Uuid,
    ) -> Result<application::ports::repository::TrackCredits, AppError> {
        let rows = sqlx::query!(
            r#"
            SELECT ar.name, ta.role as "role: domain::ArtistRole", ta.position
            FROM track_artists ta
            JOIN artists ar ON ar.id = ta.artist_id
            WHERE ta.track_id = $1
              AND ta.role IN ('composer', 'lyricist')
            ORDER BY ta.role, ta.position ASC
            "#,
            track_id
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(crate::db_err!("track.get_credits"))?;

        let mut composers = Vec::new();
        let mut lyricists = Vec::new();

        for row in rows {
            match row.role {
                domain::ArtistRole::Composer => composers.push(row.name),
                domain::ArtistRole::Lyricist => lyricists.push(row.name),
                _ => {}
            }
        }

        Ok(application::ports::repository::TrackCredits {
            composers,
            lyricists,
        })
    }
}
