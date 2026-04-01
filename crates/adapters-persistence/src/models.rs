use sqlx::FromRow;
use uuid::Uuid;
// ... DTOs mapped directly from Postgres if needed...

#[derive(FromRow)]
pub struct DbMediaAsset {
    pub id: Uuid,
    pub title: String,
    pub origin_type: String, // 'local' or 'remote'
    pub origin_rel_path: Option<String>,
    pub origin_remote_url: Option<String>,
    pub duration_ms: Option<i64>,
}
