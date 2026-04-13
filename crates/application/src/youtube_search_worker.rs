use std::sync::Arc;

use tracing::{debug, error, info};

use crate::ports::repository::YoutubeSearchRepository;
use crate::ports::search::MusicSearchPort;
use crate::ports::ytdlp::YtDlpPort;
use domain::search::SearchResult;

pub struct YoutubeSearchWorker {
    youtube_repo: Arc<dyn YoutubeSearchRepository>,
    ytdlp_port: Arc<dyn YtDlpPort>,
    pub search_port: Arc<dyn MusicSearchPort>,
    in_flight: dashmap::DashSet<String>,
}

impl YoutubeSearchWorker {
    pub fn new(
        youtube_repo: Arc<dyn YoutubeSearchRepository>,
        ytdlp_port: Arc<dyn YtDlpPort>,
        search_port: Arc<dyn MusicSearchPort>,
    ) -> Self {
        Self {
            youtube_repo,
            ytdlp_port,
            search_port,
            in_flight: dashmap::DashSet::new(),
        }
    }

    /// Background task explicitly spawned from Discord handler
    pub async fn fetch_and_cache(
        &self,
        query: String,
        cache: Arc<moka::future::Cache<String, Vec<SearchResult>>>,
    ) {
        if !self.in_flight.insert(query.clone()) {
            return;
        }

        info!(query = %query, "Background yt-dlp search fetching top 5 results");

        // Execute yt-dlp search via the configured port.
        match self.ytdlp_port.search_top_n(&query, 5).await {
            Ok(results) => {
                let mut search_results = Vec::new();
                for meta in &results {
                    let result_query = query.clone();

                    if let Err(e) = self
                        .youtube_repo
                        .upsert_search_result(&result_query, meta)
                        .await
                    {
                        error!(error = %e, video_id = %meta.video_id, "Failed to upsert youtube search result into db");
                    }

                    search_results.push(SearchResult {
                        source: "youtube_search".to_string(),
                        track_id: None,
                        youtube_video_id: Some(meta.video_id.clone()),
                        title: meta.title.clone().unwrap_or_default(),
                        artist_display: meta.uploader.clone(),
                        uploader: meta.uploader.clone(),
                        duration_ms: meta.duration_ms,
                    });
                }

                cache.insert(query.clone(), search_results.clone()).await;

                // C3: Filter out videos already promoted to `tracks` to avoid
                // re-inserting duplicate youtube_search stubs in Tantivy
                let video_ids: Vec<String> = search_results
                    .iter()
                    .filter_map(|r| r.youtube_video_id.clone())
                    .collect();

                let existing = match self.youtube_repo.find_existing_video_ids(&video_ids).await {
                    Ok(set) => set,
                    Err(e) => {
                        error!(error = %e, "Failed to check existing video IDs");
                        std::collections::HashSet::new()
                    }
                };

                let new_results: Vec<SearchResult> = search_results
                    .into_iter()
                    .filter(|r| {
                        r.youtube_video_id
                            .as_ref()
                            .is_none_or(|vid| !existing.contains(vid))
                    })
                    .collect();

                if !new_results.is_empty() {
                    if let Err(e) = self.search_port.add_search_results(new_results).await {
                        error!(error = %e, "Failed to insert youtube search results into Tantivy");
                    } else {
                        debug!(query = %query, "Successfully cached YouTube search results");
                    }
                }
            }
            Err(e) => {
                error!(error = %e, query = %query, "Failed to compute yt-dlp search");
            }
        }

        self.in_flight.remove(&query);
    }
}
