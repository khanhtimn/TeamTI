use async_trait::async_trait;
use uuid::Uuid;

use crate::db_err;
use application::LISTEN_COMPLETION_THRESHOLD;
use application::error::AppError;
use application::ports::user_library::UserLibraryPort;
use domain::track::TrackSummary;
use domain::user_library::FavouritesPage;

use crate::db::Database;

pub struct PgUserLibraryRepository {
    db: Database,
}

impl PgUserLibraryRepository {
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

#[async_trait]
impl UserLibraryPort for PgUserLibraryRepository {
    async fn add_favourite(&self, user_id: &str, track_id: Uuid) -> Result<(), AppError> {
        sqlx::query!(
            "INSERT INTO favorites (user_id, track_id) VALUES ($1, $2)
             ON CONFLICT (user_id, track_id) DO NOTHING",
            user_id,
            track_id
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("user_library.add_favourite"))?;
        Ok(())
    }

    async fn remove_favourite(&self, user_id: &str, track_id: Uuid) -> Result<(), AppError> {
        sqlx::query!(
            "DELETE FROM favorites WHERE user_id = $1 AND track_id = $2",
            user_id,
            track_id
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("user_library.remove_favourite"))?;
        Ok(())
    }

    async fn is_favourite(&self, user_id: &str, track_id: Uuid) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM favorites WHERE user_id = $1 AND track_id = $2) AS "exists!""#,
            user_id,
            track_id
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(db_err!("user_library.is_favourite"))?;
        Ok(exists)
    }

    async fn list_favourites(
        &self,
        user_id: &str,
        page: i64,
        page_size: i64,
    ) -> Result<FavouritesPage, AppError> {
        let total: i64 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM favorites WHERE user_id = $1"#,
            user_id
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(db_err!("user_library.list_favourites.count"))?;

        let offset = page * page_size;

        let rows = sqlx::query_as!(
            TrackSummaryRow,
            r#"SELECT t.id, t.title, t.artist_display, a.title AS album_title,
                    t.album_id, t.duration_ms, t.blob_location
             FROM favorites f
             JOIN tracks t ON t.id = f.track_id
             LEFT JOIN albums a ON a.id = t.album_id
             WHERE f.user_id = $1
             ORDER BY f.created_at DESC
             LIMIT $2 OFFSET $3"#,
            user_id,
            page_size,
            offset
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(db_err!("user_library.list_favourites"))?;

        Ok(FavouritesPage {
            tracks: rows.into_iter().map(Into::into).collect(),
            total,
            page,
            page_size,
        })
    }

    async fn open_listen_event(
        &self,
        user_id: &str,
        track_id: Uuid,
        guild_id: &str,
    ) -> Result<Uuid, AppError> {
        let id: Uuid = sqlx::query_scalar!(
            "INSERT INTO listen_events (user_id, track_id, guild_id)
             VALUES ($1, $2, $3)
             RETURNING id",
            user_id,
            track_id,
            guild_id,
        )
        .fetch_one(&self.db.0)
        .await
        .map_err(db_err!("user_library.open_listen_event"))?;
        Ok(id)
    }

    async fn close_dangling_events(&self, older_than_secs: i64) -> Result<u64, AppError> {
        let rows = sqlx::query!(
            "UPDATE listen_events
             SET play_duration_ms = 0, completed = false
             WHERE play_duration_ms IS NULL
               AND started_at < NOW() - make_interval(secs => $1)",
            older_than_secs as i32
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("user_library.close_dangling_events"))?;

        Ok(rows.rows_affected())
    }

    async fn close_listen_event(
        &self,
        user_id: &str,
        track_id: Uuid,
        play_duration_ms: i64,
        track_duration_ms: i64,
    ) -> Result<(), AppError> {
        let completed = if track_duration_ms > 0 {
            (play_duration_ms as f64 / track_duration_ms as f64) >= LISTEN_COMPLETION_THRESHOLD
        } else {
            false
        };

        sqlx::query!(
            "UPDATE listen_events
             SET play_duration_ms = $1, completed = $2
             WHERE user_id = $3 AND track_id = $4 AND play_duration_ms IS NULL",
            play_duration_ms,
            completed,
            user_id,
            track_id
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("user_library.close_listen_event"))?;
        Ok(())
    }

    async fn close_listen_events_for_track(
        &self,
        track_id: Uuid,
        guild_id: &str,
        play_duration_ms: i64,
        track_duration_ms: i64,
    ) -> Result<Vec<String>, AppError> {
        struct RowResult {
            user_id: String,
        }

        let completed = if track_duration_ms > 0 {
            (play_duration_ms as f64 / track_duration_ms as f64) >= LISTEN_COMPLETION_THRESHOLD
        } else {
            false
        };

        // IF not completed, we still update it but don't need to trigger affinity update downstream.
        if !completed {
            sqlx::query!(
                "UPDATE listen_events
                 SET play_duration_ms = $1, completed = $2
                 WHERE track_id = $3 AND guild_id = $4 AND play_duration_ms IS NULL",
                play_duration_ms,
                completed,
                track_id,
                guild_id
            )
            .execute(&self.db.0)
            .await
            .map_err(db_err!("user_library.close_listen_events_for_track"))?;

            return Ok(Vec::new());
        }

        let result = sqlx::query_as!(
            RowResult,
            "UPDATE listen_events
             SET play_duration_ms = $1, completed = $2
             WHERE track_id = $3 AND guild_id = $4 AND play_duration_ms IS NULL
             RETURNING user_id",
            play_duration_ms,
            completed,
            track_id,
            guild_id
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(db_err!(
            "user_library.close_listen_events_for_track_returning"
        ))?;

        Ok(result.into_iter().map(|r| r.user_id).collect())
    }

    async fn recent_history(
        &self,
        user_id: &str,
        limit: i64,
    ) -> Result<Vec<TrackSummary>, AppError> {
        let rows = sqlx::query_as!(
            TrackSummaryRow,
            r#"SELECT DISTINCT ON (t.id) t.id, t.title, t.artist_display,
                    al.title AS album_title, t.album_id, t.duration_ms, t.blob_location
             FROM listen_events le
             JOIN tracks t ON t.id = le.track_id
             LEFT JOIN albums al ON al.id = t.album_id
             WHERE le.user_id = $1
             ORDER BY t.id, le.started_at DESC
             LIMIT $2"#,
            user_id,
            limit
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(db_err!("user_library.recent_history"))?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}

// ── Helper row types ─────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct TrackSummaryRow {
    id: Uuid,
    title: String,
    artist_display: Option<String>,
    album_title: Option<String>,
    album_id: Option<Uuid>,
    duration_ms: Option<i64>,
    blob_location: String,
}

impl From<TrackSummaryRow> for TrackSummary {
    fn from(r: TrackSummaryRow) -> Self {
        Self {
            id: r.id,
            title: r.title,
            artist_display: r.artist_display,
            album_title: r.album_title,
            album_id: r.album_id,
            duration_ms: r.duration_ms,
            blob_location: Some(r.blob_location),
        }
    }
}
