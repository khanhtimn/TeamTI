use async_trait::async_trait;
use crate::db::Database;
use crate::models::DbMediaAsset;
use domain::error::DomainError;
use domain::media::{MediaAsset, MediaOrigin};
use application::ports::media_repository::MediaRepository;

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
            INSERT INTO media_assets (id, title, origin_type, origin_rel_path, origin_remote_url, duration_ms, content_hash, original_filename)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (id) DO UPDATE
            SET title = $2, origin_type = $3, origin_rel_path = $4, origin_remote_url = $5,
                duration_ms = $6, content_hash = $7, original_filename = $8
            "#,
            asset.id,
            asset.title,
            origin_type,
            origin_rel_path,
            origin_remote_url,
            asset.duration_ms.map(|d| d as i64),
            asset.content_hash,
            asset.original_filename,
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
                   content_hash, original_filename
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
                   content_hash, original_filename
            FROM media_assets WHERE content_hash = $1 LIMIT 1
            "#,
            hash
        )
        .fetch_optional(&self.db.0)
        .await
        .map_err(|e| DomainError::InvalidState(e.to_string()))?;

        Ok(row.map(row_to_asset))
    }

    async fn search(&self, query: &str, limit: i64) -> Result<Vec<MediaAsset>, DomainError> {
        let pattern = format!("%{query}%");
        let rows = sqlx::query_as!(
            DbMediaAsset,
            r#"
            SELECT id, title, origin_type, origin_rel_path, origin_remote_url, duration_ms,
                   content_hash, original_filename
            FROM media_assets
            WHERE title ILIKE $1 OR original_filename ILIKE $1
            LIMIT $2
            "#,
            pattern,
            limit
        )
        .fetch_all(&self.db.0)
        .await
        .map_err(|e| DomainError::InvalidState(e.to_string()))?;

        Ok(rows.into_iter().map(row_to_asset).collect())
    }
}
