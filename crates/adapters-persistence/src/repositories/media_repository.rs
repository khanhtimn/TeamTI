use crate::db::Database;
use crate::models::DbMediaAsset;
use application::ports::media_repository::MediaRepository;
use application::ports::search::{MediaSearchPort, MediaSearchResult};
use async_trait::async_trait;
use domain::error::DomainError;
use domain::media::{MediaAsset, MediaOrigin};
use tracing::debug;

pub struct PgMediaRepository {
    db: Database,
}

impl PgMediaRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

fn row_to_asset(row: DbMediaAsset) -> MediaAsset {
    let origin = match row.origin_type.as_str() {
        "local" => MediaOrigin::LocalManaged {
            rel_path: row.origin_rel_path.unwrap_or_default(),
        },
        _ => MediaOrigin::Remote(row.origin_remote_url.unwrap_or_default()),
    };

    MediaAsset {
        id: row.id,
        title: row.title,
        origin,
        duration_ms: row.duration_ms.map(|d| d as u64),
        content_hash: row.content_hash,
        original_filename: row.original_filename,
        artist: row.artist,
        search_text: None, // generated column, not selected in most queries
    }
}

#[async_trait]
impl MediaRepository for PgMediaRepository {
    async fn save(&self, asset: &MediaAsset) -> Result<(), DomainError> {
        let (origin_type, origin_rel_path, origin_remote_url) = match &asset.origin {
            MediaOrigin::LocalManaged { rel_path } => ("local", Some(rel_path.clone()), None),
            MediaOrigin::Remote(url) => ("remote", None, Some(url.clone())),
        };

        sqlx::query!(
            r#"
            INSERT INTO media_assets (id, title, origin_type, origin_rel_path, origin_remote_url, duration_ms, content_hash, original_filename, artist)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (id) DO UPDATE
            SET title = $2, origin_type = $3, origin_rel_path = $4, origin_remote_url = $5,
                duration_ms = $6, content_hash = $7, original_filename = $8, artist = $9
            "#,
            asset.id,
            asset.title,
            origin_type,
            origin_rel_path,
            origin_remote_url,
            asset.duration_ms.map(|d| d as i64),
            asset.content_hash,
            asset.original_filename,
            asset.artist,
        )
        .execute(&self.db.0)
        .await
        .map_err(|e| DomainError::InvalidState(e.to_string()))?;

        Ok(())
    }

    async fn find_by_id(&self, id: uuid::Uuid) -> Result<Option<MediaAsset>, DomainError> {
        let row = sqlx::query_as!(
            DbMediaAsset,
            r#"
            SELECT id, title, origin_type, origin_rel_path, origin_remote_url, duration_ms,
                   content_hash, original_filename, artist
            FROM media_assets WHERE id = $1 LIMIT 1
            "#,
            id
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(|e| DomainError::InvalidState(e.to_string()))?;

        Ok(row.map(row_to_asset))
    }

    async fn find_by_content_hash(&self, hash: &str) -> Result<Option<MediaAsset>, DomainError> {
        let row = sqlx::query_as!(
            DbMediaAsset,
            r#"
            SELECT id, title, origin_type, origin_rel_path, origin_remote_url, duration_ms,
                   content_hash, original_filename, artist
            FROM media_assets WHERE content_hash = $1 LIMIT 1
            "#,
            hash
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(|e| DomainError::InvalidState(e.to_string()))?;

        Ok(row.map(row_to_asset))
    }
}

/// Row type for the hybrid search query. Includes a computed `rank` column.
#[derive(sqlx::FromRow)]
struct SearchRow {
    id: uuid::Uuid,
    title: String,
    artist: Option<String>,
    original_filename: Option<String>,
    #[allow(dead_code)]
    rank: Option<f64>,
}

#[async_trait]
impl MediaSearchPort for PgMediaRepository {
    async fn search_assets(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MediaSearchResult>, DomainError> {
        let limit_i64 = limit as i64;

        // Empty query: return all assets ordered alphabetically by title (v1 behavior)
        if query.trim().is_empty() {
            let rows = sqlx::query_as!(
                SearchRow,
                r#"
                SELECT
                    id,
                    title,
                    artist,
                    original_filename,
                    1.0::float8 AS "rank: f64"
                FROM media_assets
                ORDER BY title ASC
                LIMIT $1
                "#,
                limit_i64,
            )
            .fetch_all(&self.db.0)
            .await
            .map_err(|e| DomainError::InvalidState(e.to_string()))?;

            debug!(
                query = query,
                results = rows.len(),
                "Media search completed (empty query, returning all)"
            );

            return Ok(rows
                .into_iter()
                .map(|r| MediaSearchResult {
                    asset_id: r.id,
                    title: r.title,
                    artist: r.artist,
                    original_filename: r.original_filename,
                })
                .collect());
        }

        // Non-empty query: rank ALL assets by confidence score, no hard filter.
        // Confidence = FTS rank (if FTS matches) + trigram word similarity.
        // Assets with zero similarity still appear — they just rank at the bottom.
        let rows = sqlx::query_as!(
            SearchRow,
            r#"
            SELECT
                id,
                title,
                artist,
                original_filename,
                (
                    CASE WHEN search_vector @@ websearch_to_tsquery('music_simple', $1)
                         THEN ts_rank_cd(search_vector, websearch_to_tsquery('music_simple', $1))
                         ELSE 0
                    END
                    + coalesce(word_similarity($1, search_text), 0)
                )::float8 AS "rank: f64"
            FROM media_assets
            ORDER BY "rank: f64" DESC, title ASC
            LIMIT $2
            "#,
            query,
            limit_i64,
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(|e| DomainError::InvalidState(e.to_string()))?;

        debug!(
            query = query,
            results = rows.len(),
            "Media search completed"
        );

        Ok(rows
            .into_iter()
            .map(|r| MediaSearchResult {
                asset_id: r.id,
                title: r.title,
                artist: r.artist,
                original_filename: r.original_filename,
            })
            .collect())
    }
}
