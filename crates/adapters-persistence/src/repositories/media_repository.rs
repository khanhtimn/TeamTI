use async_trait::async_trait;
use crate::db::Database;
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

#[async_trait]
impl MediaRepository for PgMediaRepository {
    async fn save(&self, asset: &MediaAsset) -> Result<(), DomainError> {
        let (origin_type, origin_rel_path, origin_remote_url) = match &asset.origin {
            MediaOrigin::LocalManaged { rel_path } => ("local", Some(rel_path.clone()), None),
            MediaOrigin::Remote(url) => ("remote", None, Some(url.clone())),
        };

        sqlx::query!(
            r#"
            INSERT INTO media_assets (id, title, origin_type, origin_rel_path, origin_remote_url, duration_ms)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (id) DO UPDATE 
            SET title = $2, origin_type = $3, origin_rel_path = $4, origin_remote_url = $5, duration_ms = $6
            "#,
            asset.id,
            asset.title,
            origin_type,
            origin_rel_path,
            origin_remote_url,
            asset.duration_ms.map(|d| d as i64),
        )
        .execute(&self.db.0)
        .await
        .map_err(|e| DomainError::InvalidState(e.to_string()))?;

        Ok(())
    }
}
