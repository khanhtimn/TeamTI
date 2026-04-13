use std::sync::Arc;

use tokio::sync::{Semaphore, mpsc};
use tracing::{debug, info, warn};
use uuid::Uuid;

use application::events::TrackScanned;
use application::ports::repository::TrackRepository;
use domain::{EnrichmentStatus, Track};
use shared_config::Config;

use crate::classifier::ToFingerprint;
use crate::tag_reader::read_file;

pub async fn run_fingerprint_worker(
    _config: Arc<Config>,
    track_repo: Arc<dyn TrackRepository>,
    smb_semaphore: Arc<Semaphore>,
    fp_concurrency: Arc<Semaphore>,
    mut fp_rx: mpsc::Receiver<ToFingerprint>,
    scan_tx: mpsc::Sender<TrackScanned>,
) {
    while let Some(msg) = fp_rx.recv().await {
        let repo = Arc::clone(&track_repo);
        let smb = Arc::clone(&smb_semaphore);
        let fp_sem = Arc::clone(&fp_concurrency);
        let tx = scan_tx.clone();

        tokio::spawn(async move {
            // Step 1: Acquire fp_concurrency permit.
            let fp_permit = fp_sem
                .acquire_owned()
                .await
                .expect("fp_concurrency semaphore closed");

            // Step 2: Acquire SMB permit (owned so it can cross spawn_blocking).
            let smb_permit = smb.acquire_owned().await.expect("smb_semaphore closed");

            // Step 3: spawn_blocking — SMB permit is moved in and drops on return.
            let path = msg.path.clone();
            let decode_result = tokio::task::spawn_blocking(move || {
                let _permit = smb_permit; // drops when closure returns
                read_file(&path)
            })
            .await;

            drop(fp_permit); // release fp_concurrency slot — decode is done

            // Step 4: Handle result and write to DB.
            match decode_result {
                Err(join_err) => {
                    warn!(
                        "fingerprint: spawn_blocking panic for {:?}: {join_err}",
                        msg.path
                    );
                }
                Ok(Err(e)) => {
                    warn!("fingerprint: read_file failed for {:?}: {e}", msg.path);
                }
                Ok(Ok((fp, raw_tags, duration_ms))) => {
                    let rel = msg.rel; // pre-computed by classifier
                    let mtime: chrono::DateTime<chrono::Utc> = msg.mtime.into();

                    match repo.find_by_fingerprint(&fp.fingerprint).await {
                        Err(e) => {
                            warn!("fingerprint: DB fingerprint lookup failed: {e}");
                        }

                        Ok(Some(existing)) => {
                            let same_location =
                                existing.blob_location.as_deref() == Some(rel.as_str());
                            let same_id = msg.existing_id == Some(existing.id);

                            if same_location || same_id {
                                // Same audio content, same or expected location.
                                debug!("fingerprint: mtime/size update for {rel}");
                                let _ = repo
                                    .update_file_identity(
                                        existing.id,
                                        mtime,
                                        i64::try_from(msg.size_bytes).unwrap_or_default(),
                                        &rel,
                                    )
                                    .await;
                            } else {
                                // Same audio, different path — file was moved/renamed.
                                info!(
                                    "fingerprint: moved {} → {}",
                                    existing.blob_location.as_deref().unwrap_or("?"),
                                    rel
                                );
                                let _ = repo
                                    .update_file_identity(
                                        existing.id,
                                        mtime,
                                        i64::try_from(msg.size_bytes).unwrap_or_default(),
                                        &rel,
                                    )
                                    .await;
                                // Do NOT re-enrich; identity is preserved.
                            }
                        }

                        Ok(None) => {
                            // New audio content — insert and enqueue for enrichment.
                            let title = raw_tags.title.unwrap_or_else(|| {
                                let stem = crate::text::normalize_filename_stem(&msg.path);
                                if stem.is_empty() {
                                    "Unknown".into()
                                } else {
                                    stem
                                }
                            });

                            let (youtube_video_id, source) = if rel.starts_with("youtube/") {
                                let file_stem = std::path::Path::new(&rel)
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("");
                                let vid = file_stem.rsplit('_').next().map(ToString::to_string);
                                (vid, "youtube".to_string())
                            } else {
                                (None, "local".to_string())
                            };

                            let track = Track {
                                id: Uuid::new_v4(),
                                title,
                                artist_display: raw_tags.artist,
                                album_id: None,
                                track_number: raw_tags
                                    .track_number
                                    .map(|n| i32::try_from(n).unwrap_or_default()),
                                disc_number: raw_tags
                                    .disc_number
                                    .map(|n| i32::try_from(n).unwrap_or_default()),
                                duration_ms: Some(duration_ms),
                                genres: raw_tags.genres,
                                year: raw_tags.year,
                                bpm: raw_tags.bpm,
                                isrc: raw_tags.isrc,
                                lyrics: raw_tags.lyrics,
                                bitrate: raw_tags.bitrate,
                                sample_rate: raw_tags.sample_rate,
                                channels: raw_tags.channels,
                                codec: raw_tags.codec,
                                audio_fingerprint: Some(fp.fingerprint.clone()),
                                file_modified_at: Some(mtime),
                                file_size_bytes: Some(
                                    i64::try_from(msg.size_bytes).unwrap_or_default(),
                                ),
                                blob_location: Some(rel.clone()),
                                source,
                                youtube_video_id,
                                youtube_channel_id: None,
                                youtube_uploader: None,
                                youtube_thumbnail_url: None,
                                mbid: None,
                                acoustid_id: None,
                                enrichment_status: EnrichmentStatus::Pending,
                                enrichment_confidence: None,
                                enrichment_attempts: 0,
                                enrichment_locked: false,
                                enriched_at: None,
                                created_at: chrono::Utc::now(),
                                updated_at: chrono::Utc::now(),
                                tags_written_at: None,
                                analysis_status: domain::AnalysisStatus::default(),
                                analysis_attempts: 0,
                                analysis_locked: false,
                                analyzed_at: None,
                            };

                            match repo.insert(&track).await {
                                Ok((inserted, is_inserted)) => {
                                    if is_inserted {
                                        info!(
                                            "fingerprint: indexed {} ({})",
                                            inserted.id,
                                            inserted.blob_location.as_deref().unwrap_or("?")
                                        );
                                    } else {
                                        info!(
                                            "fingerprint: audio duplicate merged for {} ({})",
                                            inserted.id,
                                            inserted.blob_location.as_deref().unwrap_or("?")
                                        );
                                    }

                                    // Always trigger reactive enrichment scans whether newly inserted or merged over
                                    // a pre-existing youtube stub payload
                                    let _ = tx
                                        .send(TrackScanned {
                                            track_id: inserted.id,
                                            fingerprint: fp.fingerprint,
                                            duration_ms: fp.duration_ms,
                                            blob_location: rel.clone(),
                                            correlation_id: msg.correlation_id,
                                        })
                                        .await;
                                }
                                Err(e) => {
                                    warn!("fingerprint: insert failed: {e}");
                                }
                            }
                        }
                    }
                }
            }
            // _fp_permit drops here, freeing the fp_concurrency slot.
        });
    }
}
