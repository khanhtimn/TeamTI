use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct MbRecordingResponse {
    #[allow(dead_code)]
    pub id: String,
    pub title: String,
    #[serde(default)]
    #[serde(rename = "artist-credit")]
    pub artist_credit: Vec<MbArtistCreditItem>,
    #[serde(default)]
    pub releases: Vec<MbRelease>,
    #[serde(default)]
    pub genres: Vec<MbGenre>,
    #[serde(default)]
    pub isrcs: Vec<String>,
    #[serde(default)]
    pub relations: Vec<MbRelation>,
}

#[derive(Debug, Deserialize)]
pub struct MbArtistCreditItem {
    pub name: String,
    pub artist: MbArtist,
    #[serde(default)]
    pub joinphrase: String,
}

#[derive(Debug, Deserialize)]
pub struct MbArtist {
    pub id: String,
    #[allow(dead_code)]
    pub name: String,
    #[serde(rename = "sort-name")]
    pub sort_name: String,
    #[serde(default)]
    pub genres: Vec<MbGenre>,
}

#[derive(Debug, Deserialize)]
pub struct MbRelease {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub date: String, // "YYYY", "YYYY-MM", or "YYYY-MM-DD"
    #[serde(default)]
    pub barcode: Option<String>,
    #[serde(default)]
    #[serde(rename = "track-count")]
    #[allow(dead_code)]
    pub track_count: Option<u32>,
    // B4 fix: added for release selection priority
    #[serde(default)]
    pub status: Option<String>, // "Official", "Bootleg", "Promotion", etc.
    #[serde(default)]
    #[serde(rename = "release-group")]
    pub release_group: Option<MbReleaseGroup>,
    #[serde(default)]
    #[serde(rename = "label-info")]
    pub label_info: Option<Vec<MbLabelInfo>>,
}

#[derive(Debug, Deserialize)]
pub struct MbLabelInfo {
    pub label: Option<MbLabel>,
}

#[derive(Debug, Deserialize)]
pub struct MbLabel {
    pub name: String,
}

// B4 fix: release group type for prioritized release selection
#[derive(Debug, Deserialize)]
pub struct MbReleaseGroup {
    #[serde(default)]
    #[serde(rename = "primary-type")]
    pub primary_type: Option<String>, // "Album", "Single", "EP", "Compilation", etc.
    #[serde(default)]
    pub genres: Vec<MbGenre>,
}

#[derive(Debug, Deserialize)]
pub struct MbGenre {
    pub name: String,
}

/// B4 fix: priority for release selection.
/// Lower is better. Official studio albums are preferred over compilations.
pub fn release_priority(r: &MbRelease) -> u8 {
    let is_official = r.status.as_deref() == Some("Official");
    let is_album = r
        .release_group
        .as_ref()
        .and_then(|rg| rg.primary_type.as_deref())
        == Some("Album");
    match (is_album, is_official) {
        (true, true) => 0,   // best: official studio album
        (false, true) => 1,  // official, not album
        (true, false) => 2,  // unofficial album
        (false, false) => 3, // other
    }
}

// --- F3: Relationship structs for work-rels and artist-rels ---

/// A relationship entry from a MusicBrainz entity.
/// Used for recording→work (type="performance") and work→artist (type="composer"/"lyricist").
#[derive(Debug, Deserialize)]
pub struct MbRelation {
    #[serde(rename = "type")]
    pub rel_type: String,
    #[serde(default)]
    pub direction: Option<String>, // "forward" or "backward"
    #[serde(default)]
    pub target: Option<String>, // "work" or "artist"
    #[serde(default)]
    pub work: Option<MbRelationWork>,
    #[serde(default)]
    pub artist: Option<MbRelationArtist>,
}

#[derive(Debug, Deserialize)]
pub struct MbRelationWork {
    pub id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MbRelationArtist {
    pub id: String,
    pub name: String,
    #[serde(rename = "sort-name")]
    #[serde(default)]
    pub sort_name: Option<String>,
}

/// Response from `work/{id}?inc=artist-rels`
#[derive(Debug, Deserialize)]
pub struct MbWorkResponse {
    #[allow(dead_code)]
    pub id: String,
    #[serde(default)]
    pub relations: Vec<MbRelation>,
}

/// Response from `release/{id}?inc=labels`
#[derive(Debug, Deserialize)]
pub struct MbReleaseResponse {
    #[allow(dead_code)]
    pub id: String,
    #[serde(default)]
    #[serde(rename = "label-info")]
    pub label_info: Vec<MbLabelInfo>,
}
