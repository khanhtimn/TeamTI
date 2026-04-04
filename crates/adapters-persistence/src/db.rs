use sqlx::{PgPool, postgres::PgPoolOptions};
use std::time::Duration;

#[derive(Clone)]
pub struct Database(pub PgPool);

impl Database {
    pub async fn connect(url: &str, pool_size: u32) -> std::result::Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(pool_size)
            .acquire_timeout(Duration::from_secs(3))
            .connect(url)
            .await?;

        Ok(Self(pool))
    }
}
