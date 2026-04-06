use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::events::{ToLastFm, ToMusicBrainz};
use crate::ports::{AlbumRepository, ArtistRepository, MusicBrainzPort, TrackRepository};
use domain::{Album, AlbumArtist, Artist, ArtistRole, EnrichmentStatus, TrackArtist};

pub struct MusicBrainzWorker {
    pub port: Arc<dyn MusicBrainzPort>,
    pub track_repo: Arc<dyn TrackRepository>,
    pub artist_repo: Arc<dyn ArtistRepository>,
    pub album_repo: Arc<dyn AlbumRepository>,
    pub failed_retry_limit: i32,
    pub fetch_work_credits: bool,
}

impl MusicBrainzWorker {
    pub async fn run(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<ToMusicBrainz>,
        lastfm_tx: mpsc::Sender<ToLastFm>,
    ) {
        while let Some(msg) = rx.recv().await {
            // Rate limiting enforced inside the port.
            let recording = match self.port.fetch_recording(&msg.mbid).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        track_id = %msg.track_id,
                        correlation_id = %msg.correlation_id,
                        error = %e,
                        "musicbrainz: fetch failed"
                    );
                    let attempts = msg.enrichment_attempts + 1;
                    let status = if attempts >= self.failed_retry_limit {
                        EnrichmentStatus::Exhausted
                    } else {
                        EnrichmentStatus::Failed
                    };
                    let _ = self
                        .track_repo
                        .update_enrichment_status(
                            msg.track_id,
                            &status,
                            attempts,
                            Some(chrono::Utc::now()),
                        )
                        .await;

                    // B3 fix: Fallback metadata pipe to allow lyrics/cover logic natively
                    if let Ok(Some(track)) = self.track_repo.find_by_id(msg.track_id).await {
                        let album_dir = std::path::Path::new(&msg.blob_location)
                            .parent()
                            .filter(|p| *p != std::path::Path::new(""))
                            .map(|p| p.to_string_lossy().into_owned());

                        let _ = lastfm_tx
                            .send(ToLastFm {
                                track_id: msg.track_id,
                                release_mbid: String::new(),
                                album_dir,
                                blob_location: msg.blob_location.clone(),
                                enrichment_attempts: attempts,
                                correlation_id: msg.correlation_id,
                                track_name: track.title,
                                artist_name: track.artist_display.unwrap_or_default(),
                                album_name: None,
                                duration_secs: msg.duration_secs,
                                artist_mbids: vec![],
                            })
                            .await;
                    }

                    continue;
                }
            };

            // --- Fetch Release Label (secondary call) ---
            // The recording endpoint doesn't include label info; we need a
            // separate release lookup. Best-effort — failure is non-fatal.
            let mut recording = recording;
            if !recording.release_mbid.is_empty() && recording.record_label.is_none() {
                match self.port.fetch_release_label(&recording.release_mbid).await {
                    Ok(label) => recording.record_label = label,
                    Err(e) => {
                        warn!(
                            track_id = %msg.track_id,
                            correlation_id = %msg.correlation_id,
                            error = %e,
                            "musicbrainz: release label fetch failed"
                        );
                    }
                }
            }

            // --- Upsert Artists ---
            let mut primary_artist_display = String::new();
            let mut upserted_artists: Vec<Artist> = Vec::new();

            for (i, credit) in recording.artist_credits.iter().enumerate() {
                let artist = Artist {
                    id: Uuid::new_v4(),
                    name: credit.name.clone(),
                    sort_name: credit.sort_name.clone(),
                    mbid: Some(credit.artist_mbid.clone()),
                    country: None,
                    created_at: chrono::Utc::now(),
                };
                let upserted = match self.artist_repo.upsert(&artist).await {
                    Ok(a) => a,
                    Err(e) => {
                        warn!(
                            track_id = %msg.track_id,
                            correlation_id = %msg.correlation_id,
                            error = %e,
                            "musicbrainz: artist upsert failed"
                        );
                        continue;
                    }
                };

                // Build display string: "Artist A feat. Artist B"
                if i == 0 {
                    primary_artist_display = credit.name.clone();
                } else if let Some(ref phrase) = credit.join_phrase {
                    primary_artist_display.push_str(phrase);
                    primary_artist_display.push_str(&credit.name);
                }

                let role = if i == 0 {
                    ArtistRole::Primary
                } else {
                    ArtistRole::Featuring
                };

                let _ = self
                    .artist_repo
                    .upsert_track_artist(&TrackArtist {
                        track_id: msg.track_id,
                        artist_id: upserted.id,
                        role,
                        position: (i + 1) as i32,
                    })
                    .await;

                upserted_artists.push(upserted);
            }

            // --- Fetch and Upsert Work Credits (Composers/Lyricists) ---
            if self.fetch_work_credits
                && let Some(work_mbid) = &recording.work_mbid
                && let Ok(work_credits) = self.port.fetch_work_credits(work_mbid).await
            {
                let mut position = 1;
                for credit in work_credits.composers {
                    let artist = Artist {
                        id: Uuid::new_v4(),
                        name: credit.name.clone(),
                        sort_name: credit.sort_name.clone(),
                        mbid: Some(credit.artist_mbid.clone()),
                        country: None,
                        created_at: chrono::Utc::now(),
                    };
                    if let Ok(upserted) = self.artist_repo.upsert(&artist).await {
                        let _ = self
                            .artist_repo
                            .upsert_track_artist(&TrackArtist {
                                track_id: msg.track_id,
                                artist_id: upserted.id,
                                role: ArtistRole::Composer,
                                position,
                            })
                            .await;
                        position += 1;
                    }
                }

                position = 1;
                for credit in work_credits.lyricists {
                    let artist = Artist {
                        id: Uuid::new_v4(),
                        name: credit.name.clone(),
                        sort_name: credit.sort_name.clone(),
                        mbid: Some(credit.artist_mbid.clone()),
                        country: None,
                        created_at: chrono::Utc::now(),
                    };
                    if let Ok(upserted) = self.artist_repo.upsert(&artist).await {
                        let _ = self
                            .artist_repo
                            .upsert_track_artist(&TrackArtist {
                                track_id: msg.track_id,
                                artist_id: upserted.id,
                                role: ArtistRole::Lyricist,
                                position,
                            })
                            .await;
                        position += 1;
                    }
                }
            }

            // --- Upsert Album ---
            let album = Album {
                id: Uuid::new_v4(),
                title: recording.release_title.clone(),
                release_year: recording.release_year,
                release_date: recording.release_date,
                total_tracks: None,
                total_discs: Some(1),
                mbid: Some(recording.release_mbid.clone()),
                record_label: recording.record_label.clone(),
                upc_barcode: recording.barcode.clone(),
                genres: if recording.genres.is_empty() {
                    None
                } else {
                    Some(recording.genres.clone())
                },
                cover_art_path: None,
                created_at: chrono::Utc::now(),
            };
            let upserted_album = match self.album_repo.upsert(&album).await {
                Ok(a) => a,
                Err(e) => {
                    warn!(
                        track_id = %msg.track_id,
                        correlation_id = %msg.correlation_id,
                        error = %e,
                        "musicbrainz: album upsert failed"
                    );
                    continue;
                }
            };

            // Reuse upserted_artists for AlbumArtist — no second DB call
            for (i, artist) in upserted_artists.iter().enumerate() {
                let _ = self
                    .artist_repo
                    .upsert_album_artist(&AlbumArtist {
                        album_id: upserted_album.id,
                        artist_id: artist.id,
                        role: if i == 0 {
                            ArtistRole::Primary
                        } else {
                            ArtistRole::Featuring
                        },
                        position: (i + 1) as i32,
                    })
                    .await;
            }

            // --- Update track record ---
            let _ = self
                .track_repo
                .update_enriched_metadata(
                    msg.track_id,
                    &recording.title,
                    &primary_artist_display,
                    Some(upserted_album.id),
                    if recording.genres.is_empty() {
                        None
                    } else {
                        Some(recording.genres)
                    },
                    recording.release_year,
                    Some(&msg.mbid),
                    Some(&msg.acoustid_id),
                    Some(msg.confidence),
                    recording.isrc.as_deref(),
                )
                .await;

            info!(
                track_id = %msg.track_id,
                correlation_id = %msg.correlation_id,
                mbid = %msg.mbid,
                "musicbrainz: match associated successfully"
            );

            // --- Fan out to Cover Art ---
            let album_dir = std::path::Path::new(&msg.blob_location)
                .parent()
                .filter(|p| *p != std::path::Path::new(""))
                .map(|p| p.to_string_lossy().into_owned());

            // Collect artist MBIDs for Last.fm lookups
            let artist_mbids: Vec<String> = upserted_artists
                .iter()
                .filter_map(|a| a.mbid.clone())
                .collect();

            let _ = lastfm_tx
                .send(ToLastFm {
                    track_id: msg.track_id,
                    release_mbid: recording.release_mbid,
                    album_dir,
                    blob_location: msg.blob_location,
                    enrichment_attempts: msg.enrichment_attempts, // DESIGN-3 carry-through
                    correlation_id: msg.correlation_id,
                    track_name: recording.title,
                    artist_name: primary_artist_display,
                    album_name: Some(recording.release_title),
                    duration_secs: msg.duration_secs,
                    artist_mbids,
                })
                .await;
        }
    }
}
