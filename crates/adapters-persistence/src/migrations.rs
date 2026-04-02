use crate::db::Database;
use tracing::info;

pub async fn run_migrations(db: &Database) -> std::result::Result<(), sqlx::Error> {
    info!("Running database migrations...");
    sqlx::migrate!("../../migrations").run(&db.0).await?;
    info!("Migrations complete.");
    Ok(())
}
