use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use notify::PollWatcher;
use notify_debouncer_full::{DebounceEventResult, new_debouncer_opt};
use tokio::sync::mpsc;
use tracing::{error, warn};

use crate::event::{FileEvent, FileEventKind};
use shared_config::Config as AppConfig;

pub struct MediaWatcher {
    // Held alive for the lifetime of the watcher.
    // Dropping this stops the poll loop.
    _debouncer: Box<dyn std::any::Any + Send>,
}

/// RAII guard: stores flag, calls store(false, Release) on drop.
/// Must be bound to a named `let` binding in every callback invocation
/// to guarantee it is not dropped until all event forwarding is complete.
struct ScanGuard(Arc<AtomicBool>);

impl Drop for ScanGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl MediaWatcher {
    pub fn start(
        config: Arc<AppConfig>,
    ) -> Result<(Self, mpsc::Receiver<FileEvent>), application::error::AppError> {
        let (tx, rx) = mpsc::channel::<FileEvent>(2048);
        let scan_in_progress = Arc::new(AtomicBool::new(false));
        let watch_path = config.media_root.clone();
        let poll_interval = Duration::from_secs(config.scan_interval_secs);
        // 5-second debounce absorbs chunked writes and rapid successive events.
        let debounce_window = Duration::from_secs(5);

        let flag = Arc::clone(&scan_in_progress);
        let tx_cb = tx.clone();

        let callback = move |result: DebounceEventResult| {
            // --- Overlap guard: try to set flag from false → true ---
            // If already true, a previous batch is still forwarding. Skip.
            if flag
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                warn!("watcher: poll cycle skipped — previous batch still in progress");
                return;
            }
            // SAFETY: guard must be a named binding. If it were a temporary,
            // it would drop immediately, clearing the flag before forwarding
            // is complete.
            #[allow(unused_variables)]
            let _guard = ScanGuard(Arc::clone(&flag));

            match result {
                Err(errors) => {
                    for e in errors {
                        error!("watcher: notify error: {e}");
                    }
                    // _guard drops here, clearing the flag
                }
                Ok(events) => {
                    for debounced in events {
                        let kind = match debounced.kind {
                            notify::EventKind::Remove(_) => FileEventKind::Remove,
                            _ => FileEventKind::CreateOrModify,
                        };
                        for path in debounced.event.paths {
                            // blocking_send: this callback runs on a std::thread,
                            // not inside the Tokio runtime.
                            if tx_cb
                                .blocking_send(FileEvent {
                                    path,
                                    kind: kind.clone(),
                                })
                                .is_err()
                            {
                                // Receiver dropped — bot is shutting down.
                                return;
                            }
                        }
                    }
                    // _guard drops here after all events are forwarded
                }
            }
        };

        let notify_config = notify::Config::default().with_poll_interval(poll_interval);

        // PollWatcher ONLY. The type parameter is explicit and mandatory.
        // Never substitute RecommendedWatcher here.
        let mut debouncer =
            new_debouncer_opt::<_, PollWatcher, notify_debouncer_full::RecommendedCache>(
                debounce_window,
                None,
                callback,
                notify_debouncer_full::RecommendedCache::default(),
                notify_config,
            )
            .map_err(|e| {
                let msg = e.to_string();
                application::error::WatcherError::new(msg, Some(Box::new(e)))
            })?;

        debouncer
            .watch(&watch_path, notify::RecursiveMode::Recursive)
            .map_err(|e| {
                let msg = e.to_string();
                application::error::WatcherError::new(msg, Some(Box::new(e)))
            })?;

        // Box<dyn Any> erases the concrete Debouncer type.
        // The debouncer is kept alive until MediaWatcher is dropped.
        Ok((
            Self {
                _debouncer: Box::new(debouncer),
            },
            rx,
        ))
    }
}
