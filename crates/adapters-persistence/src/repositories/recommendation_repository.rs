use async_trait::async_trait;
use uuid::Uuid;

use crate::db_err;
use application::error::AppError;
use application::ports::recommendation::RecommendationPort;
use domain::track::TrackSummary;

use crate::db::Database;

/// Recommendation weights — defined as constants, tunable later.
const W_GENRE: f64 = 0.4;
const W_ARTIST: f64 = 0.3;
const W_POPULAR: f64 = 0.2;
const W_FAV: f64 = 0.1;

pub struct PgRecommendationRepository {
    db: Database,
}

impl PgRecommendationRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

#[async_trait]
impl RecommendationPort for PgRecommendationRepository {
    async fn recommend(
        &self,
        user_id: &str,
        seed_track_id: Option<Uuid>,
        exclude: &[Uuid],
        limit: usize,
    ) -> Result<Vec<TrackSummary>, AppError> {
        // Multi-factor scoring query:
        // 1. Genre overlap with user's genre affinity (from completed listens + favourites)
        // 2. Artist affinity from listen history
        // 3. Global play count (cold start signal)
        // 4. Favourites similarity
        //
        // TODO: If this query is too slow (>200ms), fall back to globally most-played + random.
        // For Pass 3, we use a pragmatic approach: score in SQL with CTEs.

        let exclude_ids: Vec<Uuid> = exclude.to_vec();

        let rows = sqlx::query_as!(
            TrackSummaryRow,
            r#"
            WITH user_genres AS (
                -- Top genres from completed listens + favourites
                SELECT UNNEST(t.genres) AS genre, COUNT(*) AS cnt
                FROM listen_events le
                JOIN tracks t ON t.id = le.track_id
                WHERE le.user_id = $1 AND le.completed = true AND t.genres IS NOT NULL
                GROUP BY genre
                UNION ALL
                SELECT UNNEST(t.genres) AS genre, COUNT(*) AS cnt
                FROM favorites f
                JOIN tracks t ON t.id = f.track_id
                WHERE f.user_id = $1 AND t.genres IS NOT NULL
                GROUP BY genre
            ),
            genre_affinity AS (
                SELECT genre, SUM(cnt) AS weight
                FROM user_genres
                GROUP BY genre
                ORDER BY weight DESC
                LIMIT 10
            ),
            user_artists AS (
                -- Top artists by completed listen count
                SELECT t.artist_display AS artist, COUNT(*) AS cnt
                FROM listen_events le
                JOIN tracks t ON t.id = le.track_id
                WHERE le.user_id = $1 AND le.completed = true AND t.artist_display IS NOT NULL
                GROUP BY t.artist_display
                ORDER BY cnt DESC
                LIMIT 10
            ),
            global_popular AS (
                -- Global play counts
                SELECT track_id, COUNT(*) AS play_count
                FROM listen_events
                WHERE completed = true
                GROUP BY track_id
            ),
            seed_genres AS (
                -- Genres from the seed track (if provided)
                SELECT UNNEST(genres) AS genre
                FROM tracks
                WHERE id = $2
            ),
            candidates AS (
                SELECT DISTINCT t.id, t.title, t.artist_display,
                       al.title AS album_title, t.album_id, t.duration_ms, t.blob_location,
                       -- Genre overlap score
                       COALESCE((
                           SELECT SUM(ga.weight)
                           FROM genre_affinity ga
                           WHERE ga.genre = ANY(t.genres)
                       ), 0) AS genre_score,
                       -- Seed genre match
                       COALESCE((
                           SELECT COUNT(*)
                           FROM seed_genres sg
                           WHERE sg.genre = ANY(t.genres)
                       ), 0) AS seed_genre_score,
                       -- Artist affinity
                       COALESCE((
                           SELECT ua.cnt
                           FROM user_artists ua
                           WHERE ua.artist = t.artist_display
                       ), 0) AS artist_score,
                       -- Global popularity
                       COALESCE(gp.play_count, 0) AS popular_score,
                       -- Is favourited
                       CASE WHEN EXISTS(
                           SELECT 1 FROM favorites f
                           WHERE f.user_id = $1 AND f.track_id = t.id
                       ) THEN 1 ELSE 0 END AS fav_score
                FROM tracks t
                LEFT JOIN albums al ON al.id = t.album_id
                LEFT JOIN global_popular gp ON gp.track_id = t.id
                WHERE t.id != ALL($3::uuid[])
                  AND (t.enrichment_status = 'done' OR t.enrichment_status = 'pending')
            )
            SELECT id, title, artist_display, album_title, album_id, duration_ms, blob_location
            FROM candidates
            ORDER BY (
                $4 * genre_score +
                $4 * seed_genre_score +
                $5 * artist_score +
                $6 * popular_score +
                $7 * fav_score
            ) DESC, RANDOM()
            LIMIT $8
            "#,
            user_id,
            seed_track_id as _,
            &exclude_ids as _,
            W_GENRE as _,
            W_ARTIST as _,
            W_POPULAR as _,
            W_FAV as _,
            limit as i64
        )
        .fetch_all(&self.db.0)
        .await;

        match rows {
            Ok(rows) if !rows.is_empty() => Ok(rows.into_iter().map(Into::into).collect()),
            Ok(_) | Err(_) => {
                // Fallback: globally most-played + random
                // TODO: Improve scoring algorithm in Pass 3.1
                tracing::debug!(
                    user_id,
                    operation = "recommendation.fallback",
                    "using globally most-played fallback"
                );
                let fallback = sqlx::query_as!(
                    TrackSummaryRow,
                    r#"SELECT t.id, t.title, t.artist_display,
                            al.title AS album_title, t.album_id, t.duration_ms, t.blob_location
                     FROM tracks t
                     LEFT JOIN albums al ON al.id = t.album_id
                     LEFT JOIN (
                         SELECT track_id, COUNT(*) AS cnt
                         FROM listen_events
                         WHERE completed = true
                         GROUP BY track_id
                     ) gp ON gp.track_id = t.id
                     WHERE t.id != ALL($1::uuid[])
                     ORDER BY COALESCE(gp.cnt, 0) DESC, RANDOM()
                     LIMIT $2"#,
                    &exclude_ids,
                    limit as i64
                )
                .fetch_all(&self.db.0)
                .await
                .map_err(db_err!("recommendation.fallback"))?;

                Ok(fallback.into_iter().map(Into::into).collect())
            }
        }
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
    duration_ms: Option<i32>,
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
