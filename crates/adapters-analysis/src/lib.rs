//! Adapter: audio analysis via bliss-audio with Symphonia decoder.
//!
//! The analysis is CPU-bound, so we use `spawn_blocking` to avoid
//! blocking the async runtime. The caller (AnalysisWorker) bounds
//! concurrency via a JoinSet.

use async_trait::async_trait;
use std::path::PathBuf;

use application::AppError;
use application::error::AnalysisErrorKind;
use application::ports::AudioAnalysisPort;

// bliss-audio: Symphonia-backed decoder (no ffmpeg dependency).
use bliss_audio::NUMBER_FEATURES;
use bliss_audio::decoder::Decoder as DecoderTrait;
use bliss_audio::decoder::symphonia::SymphoniaDecoder;

/// Compile-time assertion: our DB schema uses vector(23).
/// bliss-audio Version2 (LATEST) uses 23 features:
/// If bliss-audio changes its feature count, this will fail to compile.
///   10 timbral (Tempo, Zcr, Centroid×2, Rolloff×2, Flatness×2, Loudness×2)
///   + 13 chroma (IC1-6, triads×4, norms×2, ratio).
const _: () = assert!(
    NUMBER_FEATURES == 23,
    "bliss-audio NUMBER_FEATURES != 23 — update vector(N) in migration"
);

pub const BLISS_VECTOR_DIMS: usize = NUMBER_FEATURES;

pub struct BlissAnalysisAdapter;

impl BlissAnalysisAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BlissAnalysisAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AudioAnalysisPort for BlissAnalysisAdapter {
    async fn analyse_track(&self, full_path: &str) -> Result<Vec<f32>, AppError> {
        let path = PathBuf::from(full_path);

        if !path.exists() {
            return Err(AppError::Analysis {
                kind: AnalysisErrorKind::FileNotFound,
                detail: format!("file not found: {full_path}"),
            });
        }

        let path_clone = path.clone();
        let result =
            tokio::task::spawn_blocking(move || SymphoniaDecoder::song_from_path(&path_clone))
                .await;

        match result {
            Ok(Ok(song)) => {
                // song.analysis is an Analysis struct with as_vec() or indexing
                let vector: Vec<f32> = song.analysis.as_arr1().to_vec();
                debug_assert_eq!(vector.len(), NUMBER_FEATURES);
                Ok(vector)
            }
            Ok(Err(bliss_err)) => Err(AppError::Analysis {
                kind: AnalysisErrorKind::DecodeFailed,
                detail: format!("bliss analysis failed for {full_path}: {bliss_err}"),
            }),
            Err(join_err) => Err(AppError::Analysis {
                kind: AnalysisErrorKind::TaskPanicked,
                detail: format!("analysis task panicked for {full_path}: {join_err}"),
            }),
        }
    }
}
