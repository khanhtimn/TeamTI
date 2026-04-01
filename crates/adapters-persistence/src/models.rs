use sqlx::FromRow;
use uuid::Uuid;

#[derive(FromRow)]
pub struct DbMediaAsset {
    pub id: Uuid,
    pub title: String,
    pub origin_type: String,
    pub origin_rel_path: Option<String>,
    pub origin_remote_url: Option<String>,
    pub duration_ms: Option<i64>,
    pub content_hash: Option<String>,
    pub original_filename: Option<String>,
}
