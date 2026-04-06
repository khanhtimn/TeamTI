//! Lifecycle worker: consumes TrackLifecycleEvent messages from the voice
//! adapter and dispatches business logic (listen events, radio refill).
//!
//! This keeps the voice adapter free of database/port dependencies.

use std::sync::Arc;
use uuid::Uuid;

use adapters_voice::lifecycle::{TrackLifecycleEvent, TrackLifecycleRx, TrackLifecycleTx};
use adapters_voice::player::enqueue_track;
use adapters_voice::state::QueuedTrack;
use adapters_voice::state_map::GuildStateMap;
use adapters_voice::track_event_handler::post_now_playing;
use application::RADIO_BATCH_SIZE;
use application::ports::recommendation::RecommendationPort;
use application::ports::repository::TrackRepository;
use application::ports::user_library::UserLibraryPort;
use domain::analysis::MoodWeight;

#[allow(clippy::too_many_arguments)]
pub async fn run_lifecycle_worker(
    mut rx: TrackLifecycleRx,
    user_library_port: Arc<dyn UserLibraryPort>,
    recommendation_port: Arc<dyn RecommendationPort>,
    track_repo: Arc<dyn TrackRepository>,
    guild_state: Arc<GuildStateMap>,
    songbird: Arc<songbird::Songbird>,
    http: Arc<serenity::all::Http>,
    cache: Arc<serenity::all::Cache>,
    media_root: std::path::PathBuf,
    auto_leave_secs: u64,
    lifecycle_tx: TrackLifecycleTx,
) {
    while let Some(event) = rx.recv().await {
        match event {
            TrackLifecycleEvent::TrackStarted {
                guild_id,
                track_id,
                track_duration_ms: _,
                users_in_channel,
            } => {
                let guild_id_str = guild_id.to_string();
                for user_id in &users_in_channel {
                    if let Err(e) = user_library_port
                        .open_listen_event(user_id, track_id, &guild_id_str)
                        .await
                    {
                        tracing::warn!(
                            user_id,
                            track_id = %track_id,
                            error = %e,
                            operation = "lifecycle.open_listen_event",
                            "failed to open listen event"
                        );
                    }
                }
            }

            TrackLifecycleEvent::TrackEnded {
                guild_id,
                track_id,
                track_duration_ms,
                play_duration_ms,
            } => {
                let track_dur = track_duration_ms.unwrap_or(0);

                let guild_id_str = guild_id.to_string();
                match user_library_port
                    .close_listen_events_for_track(
                        track_id,
                        &guild_id_str,
                        play_duration_ms,
                        track_dur,
                    )
                    .await
                {
                    Ok(user_ids) => {
                        for user_id in user_ids {
                            let _ = lifecycle_tx.send(TrackLifecycleEvent::AffinityUpdate {
                                guild_id,
                                user_id,
                                track_id,
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            track_id = %track_id,
                            error = %e,
                            operation = "lifecycle.close_listen_events_bulk",
                            "failed to close dangling listen events"
                        );
                    }
                }
            }

            TrackLifecycleEvent::RadioRefillNeeded {
                guild_id,
                user_id,
                seed_track_id,
            } => {
                // Collect current queue track IDs to exclude
                let exclude: Vec<Uuid> = guild_state
                    .get(&guild_id)
                    .map(|s| {
                        s.try_lock()
                            .ok()
                            .map(|state| {
                                state
                                    .meta_queue
                                    .iter()
                                    .map(|t| t.track_id)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();

                // Get seed vector + user centroid for mood-aware recommendations
                let seed_vector = if let Some(sid) = seed_track_id {
                    recommendation_port
                        .get_bliss_vector(sid)
                        .await
                        .ok()
                        .flatten()
                } else {
                    None
                };
                let mood_weight = if let Some(ref vector) = seed_vector {
                    mood_weight_for_track(vector)
                } else {
                    MoodWeight::BALANCED
                };

                match recommendation_port
                    .recommend(
                        &user_id,
                        seed_track_id,
                        seed_vector,
                        mood_weight,
                        &exclude,
                        RADIO_BATCH_SIZE,
                    )
                    .await
                {
                    Ok(tracks) if !tracks.is_empty() => {
                        tracing::debug!(
                            guild_id = %guild_id,
                            count = tracks.len(),
                            operation = "lifecycle.radio_refill",
                            "radio refill: enqueueing recommended tracks"
                        );

                        for summary in tracks {
                            let queued = QueuedTrack::from(summary);

                            // Add to our metadata queue first
                            let state_lock = guild_state
                                .entry(guild_id)
                                .or_insert_with(|| {
                                    Arc::new(tokio::sync::Mutex::new(
                                        adapters_voice::state::GuildMusicState::new(),
                                    ))
                                })
                                .clone();

                            let is_front = {
                                let mut state = state_lock.lock().await;
                                let empty = state.meta_queue.is_empty();
                                state.meta_queue.push_back(queued.clone());
                                if empty {
                                    state.cancel_auto_leave();
                                }
                                empty
                            };

                            // Enqueue silently — no message unless it's the front
                            if let Err(e) = enqueue_track(
                                &songbird,
                                guild_id,
                                &queued,
                                &media_root,
                                &http,
                                &cache,
                                auto_leave_secs,
                                &guild_state,
                                lifecycle_tx.clone(),
                            )
                            .await
                            {
                                tracing::warn!(
                                    guild_id = %guild_id,
                                    track_id = %queued.track_id,
                                    error = %e,
                                    operation = "lifecycle.radio_enqueue",
                                    "failed to enqueue radio track"
                                );
                                // Roll back metadata push
                                let mut state = state_lock.lock().await;
                                state.meta_queue.pop_back();
                            } else if is_front
                                && let Some(channel_id) =
                                    { state_lock.lock().await.text_channel_id }
                            {
                                post_now_playing(
                                    &http,
                                    channel_id,
                                    guild_id,
                                    &state_lock,
                                    Some(&queued),
                                    None,
                                )
                                .await;
                            }
                        }
                    }
                    Ok(_) => {
                        tracing::debug!(
                            guild_id = %guild_id,
                            operation = "lifecycle.radio_empty",
                            "no recommendations available for radio refill"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            guild_id = %guild_id,
                            error = %e,
                            operation = "lifecycle.radio_recommend_failed",
                            "radio recommendation failed"
                        );
                    }
                }
            }

            TrackLifecycleEvent::AffinityUpdate {
                guild_id,
                user_id,
                track_id,
            } => {
                // Update genre stats
                if let Ok(Some(track)) = track_repo.find_by_id(track_id).await
                    && let Some(ref genres) = track.genres
                    && let Err(e) = recommendation_port
                        .update_genre_stats(&user_id, genres)
                        .await
                {
                    tracing::debug!(
                        user_id,
                        error = %e,
                        operation = "lifecycle.genre_stats",
                        "failed to update genre stats"
                    );
                }

                // Update guild track popularity
                let guild_id_str = guild_id.to_string();
                if let Err(e) = recommendation_port
                    .update_guild_track_stats(&guild_id_str, track_id)
                    .await
                {
                    tracing::debug!(
                        track_id = %track_id,
                        error = %e,
                        operation = "lifecycle.guild_stats",
                        "failed to update guild track stats"
                    );
                }

                // Refresh affinities (non-blocking, non-fatal)
                if let Err(e) = recommendation_port.refresh_affinities(&user_id, 200).await {
                    tracing::debug!(
                        user_id,
                        error = %e,
                        operation = "lifecycle.refresh_affinities",
                        "failed to refresh affinities"
                    );
                }
            }
        }
    }
}
// TODO: verify bliss v2 feature vector indices before enabling
// mood-aware weighting. Defaulting to BALANCED until confirmed.
fn mood_weight_for_track(_bliss_vector: &[f32]) -> MoodWeight {
    MoodWeight::BALANCED
}
