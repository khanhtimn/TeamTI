use async_trait::async_trait;
use uuid::Uuid;

use crate::db_err;
use application::error::AppError;
use application::ports::recommendation::RecommendationPort;
use domain::analysis::MoodWeight;
use domain::track::TrackSummary;

use crate::db::Database;

pub struct PgRecommendationRepository {
    db: Database,
}

impl PgRecommendationRepository {
    #[must_use]
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
        seed_vector: Option<Vec<f32>>,
        mood_weight: MoodWeight,
        exclude: &[Uuid],
        limit: usize,
    ) -> Result<Vec<TrackSummary>, AppError> {
        let exclude_ids: Vec<Uuid> = exclude.to_vec();
        let w_acoustic = f64::from(mood_weight.acoustic);
        let w_taste = f64::from(mood_weight.taste);

        // Convert vectors to pgvector::Vector for binding
        let seed_vec = seed_vector.map(pgvector::Vector::from);

        // Pass 4 multi-factor recommendation CTE.
        // Uses acoustic similarity (pgvector), taste affinity (genre+artist),
        // favourites boost, and Last.fm graph proximity.
        //
        // Uses raw sqlx::query_as because pgvector types need runtime binding.
        let rows = sqlx::query_as!(
            TrackSummaryRow,
            r#"
            WITH user_genres AS (
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
                SELECT genre, SUM(cnt)::float8 / MAX(SUM(cnt)::float8) OVER () AS weight
                FROM user_genres
                GROUP BY genre
                ORDER BY weight DESC
                LIMIT 10
            ),
            user_artists AS (
                SELECT t.artist_display AS artist, COUNT(*)::float8 / MAX(COUNT(*)::float8) OVER () AS weight
                FROM listen_events le
                JOIN tracks t ON t.id = le.track_id
                WHERE le.user_id = $1 AND le.completed = true AND t.artist_display IS NOT NULL
                GROUP BY t.artist_display
                ORDER BY weight DESC
                LIMIT 10
            ),
            user_artist_mbids AS (
                SELECT DISTINCT ta2.artist_id, ar.mbid
                FROM listen_events le2
                JOIN track_artists ta2 ON ta2.track_id = le2.track_id
                JOIN artists ar ON ar.id = ta2.artist_id
                WHERE le2.user_id = $1 AND le2.completed = true AND ar.mbid IS NOT NULL
                LIMIT 20
            ),
            seed_genres AS (
                SELECT UNNEST(genres) AS genre FROM tracks WHERE id = $2
            ),
            candidates AS (
                SELECT DISTINCT t.id, t.title, t.artist_display,
                       al.title AS album_title, t.album_id, t.duration_ms, t.blob_location,
                       -- Genre overlap
                       COALESCE((
                           SELECT SUM(ga.weight) FROM genre_affinity ga WHERE ga.genre = ANY(t.genres)
                       ), 0)::float8 AS taste_score,
                       -- Seed genre match
                       COALESCE((
                           SELECT COUNT(*)::float8 / NULLIF((SELECT COUNT(*)::float8 FROM seed_genres), 0.0) FROM seed_genres sg WHERE sg.genre = ANY(t.genres)
                       ), 0.0)::float8 AS seed_genre_score,
                       -- Artist affinity
                       COALESCE((
                           SELECT ua.weight FROM user_artists ua WHERE ua.artist = t.artist_display
                       ), 0.0)::float8 AS artist_score,
                       -- Favourites boost
                       CASE WHEN EXISTS(
                           SELECT 1 FROM favorites f WHERE f.user_id = $1 AND f.track_id = t.id
                       ) THEN 1.0 ELSE 0.0 END AS fav_score,
                       -- Acoustic similarity (pgvector L2 distance, inverted to score)
                       CASE 
                           WHEN t.bliss_vector IS NOT NULL AND $3::vector IS NOT NULL
                               THEN 1.0 / (1.0 + (t.bliss_vector <-> $3::vector))
                           WHEN t.bliss_vector IS NOT NULL AND uac.centroid IS NOT NULL
                               THEN 1.0 / (1.0 + (t.bliss_vector <-> uac.centroid))
                           ELSE 0.0
                       END AS acoustic_score,
                       -- Last.fm graph proximity
                       COALESCE((
                           SELECT MAX(sa.similarity_score)
                           FROM similar_artists sa
                           JOIN track_artists ta ON ta.track_id = t.id
                           JOIN artists ar ON ar.id = ta.artist_id
                           WHERE ar.mbid IS NOT NULL
                             AND sa.source_mbid IN (SELECT mbid FROM user_artist_mbids)
                             AND sa.similar_mbid = ar.mbid
                       ), 0)::float8 AS lastfm_score
                FROM tracks t
                LEFT JOIN albums al ON al.id = t.album_id
                LEFT JOIN user_acoustic_centroids uac ON uac.user_id = $1
                WHERE t.id != ALL($4::uuid[])
                  AND (t.enrichment_status = 'done' OR t.enrichment_status = 'pending')
                  AND t.id NOT IN (
                      SELECT track_id FROM listen_events 
                      WHERE user_id = $1 AND started_at > now() - interval '3 hours'
                  )
            )
            SELECT id, title, artist_display, album_title, album_id, duration_ms, blob_location
            FROM candidates
            ORDER BY (
                (
                    $5 * (taste_score + seed_genre_score + artist_score + fav_score) +
                    $6 * acoustic_score +
                    0.15 * lastfm_score
                ) * (0.75 + RANDOM() * 0.5)
            ) DESC
            LIMIT $7
            "#,
            user_id,
            seed_track_id as Option<Uuid>,
            seed_vec as Option<pgvector::Vector>,
            &exclude_ids,
            w_taste,
            w_acoustic,
            limit as i64
        )
        .fetch_all(&self.db.0)
        .await;

        match rows {
            Ok(rows) if !rows.is_empty() => Ok(rows.into_iter().map(Into::into).collect()),
            Ok(_) | Err(_) => {
                // Fallback: globally most-played + random
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

    async fn refresh_affinities(&self, user_id: &str, _limit: usize) -> Result<(), AppError> {
        // Phase 1: simple implementation — clear + reinsert from recommend() output.
        // This is a pragmatic approach for now; a full materialized view would be
        // more efficient at scale.
        sqlx::query!(
            "DELETE FROM user_track_affinities WHERE user_id = $1",
            user_id,
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("recommendation.refresh_affinities.delete"))?;

        // Also compute and cache the user's acoustic centroid directly inside
        // the refresh lifecycle (Option B architectural alignment).
        let _ = self.compute_user_centroid(user_id).await?;

        // For now, affinities are computed lazily at recommend() time.
        // A future pass can precompute and upsert top-K candidates here.
        Ok(())
    }

    async fn update_genre_stats(&self, user_id: &str, genres: &[String]) -> Result<(), AppError> {
        // Update genre play counts for the current calendar month
        for genre in genres {
            sqlx::query!(
                r#"
                INSERT INTO user_genre_stats (user_id, genre, play_count, period_start, period_end)
                VALUES (
                    $1, $2, 1,
                    date_trunc('month', now()),
                    date_trunc('month', now()) + INTERVAL '1 month'
                )
                ON CONFLICT (user_id, genre, period_start)
                DO UPDATE SET play_count = user_genre_stats.play_count + 1
                "#,
                user_id,
                genre,
            )
            .execute(&self.db.0)
            .await
            .map_err(db_err!("recommendation.update_genre_stats"))?;
        }
        Ok(())
    }

    async fn update_guild_track_stats(
        &self,
        guild_id: &str,
        track_id: Uuid,
    ) -> Result<(), AppError> {
        sqlx::query!(
            r#"
            INSERT INTO guild_track_stats (guild_id, track_id, play_count, period_start, period_end)
            VALUES (
                $1, $2, 1,
                date_trunc('month', now()),
                date_trunc('month', now()) + INTERVAL '1 month'
            )
            ON CONFLICT (guild_id, track_id, period_start)
            DO UPDATE SET play_count = guild_track_stats.play_count + 1
            "#,
            guild_id,
            track_id,
        )
        .execute(&self.db.0)
        .await
        .map_err(db_err!("recommendation.update_guild_track_stats"))?;
        Ok(())
    }

    async fn get_bliss_vector(&self, track_id: Uuid) -> Result<Option<Vec<f32>>, AppError> {
        let row = sqlx::query_scalar!(
            r#"SELECT bliss_vector as "bliss_vector: pgvector::Vector" FROM tracks WHERE id = $1"#,
            track_id
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(db_err!("recommendation.get_bliss_vector"))?;

        Ok(row.flatten().map(|vec| vec.to_vec()))
    }

    async fn compute_user_centroid(&self, user_id: &str) -> Result<Option<Vec<f32>>, AppError> {
        // Compute the average bliss vector across the user's completed listens
        // and favourites (tracks that have been analysed).
        let row = sqlx::query_scalar!(
            r#"
            WITH new_centroid AS (
                SELECT avg(t.bliss_vector)::vector(23) AS centroid
                FROM (
                    SELECT DISTINCT track_id FROM listen_events
                    WHERE user_id = $1 AND completed = true
                    UNION
                    SELECT track_id FROM favorites WHERE user_id = $1
                ) user_tracks
                JOIN tracks t ON t.id = user_tracks.track_id
                WHERE t.bliss_vector IS NOT NULL
            )
            INSERT INTO user_acoustic_centroids (user_id, centroid, computed_at)
            SELECT $1, nc.centroid, now()
            FROM new_centroid nc
            WHERE nc.centroid IS NOT NULL
            ON CONFLICT (user_id) DO UPDATE
            SET centroid = EXCLUDED.centroid, computed_at = EXCLUDED.computed_at
            RETURNING centroid as "centroid: pgvector::Vector"
            "#,
            user_id
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(db_err!("recommendation.compute_user_centroid"))?;

        Ok(row.flatten().map(|vec| vec.to_vec()))
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
    blob_location: Option<String>,
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
            blob_location: r.blob_location,
        }
    }
}
