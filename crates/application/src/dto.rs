use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalMediaRegistrationResult {
    pub asset_id: uuid::Uuid,
    pub title: String,
}
