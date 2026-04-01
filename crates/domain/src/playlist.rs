use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: Uuid,
    pub name: String,
    pub owner_id: u64,
}

#[derive(Debug, Clone)]
pub struct PlaylistItem {
    pub id: Uuid,
    pub playlist_id: Uuid,
    pub asset_id: Uuid,
    pub position: i32,
}
