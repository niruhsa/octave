//! Learned-embedding extractor (Phase 12C) — optional ONNX audio encoder.
//!
//! Selected at runtime by `FINGERPRINT_MODEL` (a path to an `.onnx` audio
//! encoder such as OpenL3 / MusiCNN / a CLAP audio tower). It produces a 512–
//! 1280-dim learned embedding — markedly better "sounds like" quality than the
//! DSP baseline. Bumping the `model_version` auto-re-analyzes existing rows.
//!
//! Compiled only with the `onnx` cargo feature (it pulls in the heavy `ort`
//! ONNX-Runtime dependency). Without the feature, [`try_build`] returns `None`
//! and the service transparently falls back to the DSP extractor — so the
//! default build stays lean and the env var degrades gracefully.

use std::path::Path;
use std::sync::Arc;

use super::extractor::FeatureExtractor;

/// Try to construct the ONNX extractor for `model_path`. `None` when the `onnx`
/// feature isn't built, or when the model fails to load.
#[cfg(feature = "onnx")]
pub fn try_build(model_path: &Path) -> Option<Arc<dyn FeatureExtractor>> {
    match imp::OnnxExtractor::load(model_path) {
        Ok(ext) => Some(Arc::new(ext)),
        Err(e) => {
            tracing::warn!(model = %model_path.display(), error = %e, "ONNX model load failed");
            None
        }
    }
}

#[cfg(not(feature = "onnx"))]
pub fn try_build(_model_path: &Path) -> Option<Arc<dyn FeatureExtractor>> {
    None
}

#[cfg(feature = "onnx")]
mod imp {
    use std::path::Path;

    use async_trait::async_trait;
    use ort::session::Session;
    use ort::value::Value;

    use crate::error::{AppError, Result};
    use crate::services::fingerprint::extractor::FeatureExtractor;

    /// Sample rate the model expects. OpenL3/MusiCNN-family encoders are trained
    /// on mono audio resampled to this; override per model if needed.
    const MODEL_RATE: u32 = 48_000;
    /// Seconds of audio fed to the model (a representative central window).
    const WINDOW_SECS: usize = 10;

    pub struct OnnxExtractor {
        session: std::sync::Mutex<Session>,
        model_version: String,
        dims: usize,
    }

    impl OnnxExtractor {
        pub fn load(model_path: &Path) -> Result<Self> {
            let session = Session::builder()
                .and_then(|mut b| b.commit_from_file(model_path))
                .map_err(|e| {
                    AppError::Config(format!("ONNX load {}: {e}", model_path.display()))
                })?;
            // The model_version embeds the file stem so different models don't
            // collide; the analysis pass re-analyzes on a change.
            let stem = model_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("onnx");
            // The advertised `dims` is informational (the analysis pass stores the
            // *actual* embedding length per row); default to 512 (OpenL3) — the
            // real length is whatever the model outputs at inference time.
            let dims = 512;
            Ok(Self {
                session: std::sync::Mutex::new(session),
                model_version: format!("onnx-{stem}-{dims}"),
                dims,
            })
        }
    }

    #[async_trait]
    impl FeatureExtractor for OnnxExtractor {
        fn model_version(&self) -> &str {
            &self.model_version
        }
        fn dims(&self) -> usize {
            self.dims
        }
        async fn extract(&self, path: &Path) -> Result<Vec<f32>> {
            let path = path.to_path_buf();
            let pcm = tokio::task::spawn_blocking(move || {
                super::super::decode::decode_mono(&path).map_err(|e| match e {
                    super::super::decode::DecodeError::UnsupportedCodec => {
                        AppError::InvalidArgument(format!("unanalyzable: {e}"))
                    }
                    other => AppError::Internal(format!("decode: {other}")),
                })
            })
            .await
            .map_err(|e| AppError::Internal(format!("decode join: {e}")))??;

            let mono = resample(&pcm.samples, pcm.sample_rate, MODEL_RATE);
            let want = MODEL_RATE as usize * WINDOW_SECS;
            let input = center_window(&mono, want);

            let mut session = self
                .session
                .lock()
                .map_err(|_| AppError::Internal("ONNX session poisoned".into()))?;
            let tensor = Value::from_array(([1usize, input.len()], input))
                .map_err(|e| AppError::Internal(format!("ONNX input: {e}")))?;
            let outputs = session
                .run(ort::inputs!["audio" => tensor])
                .map_err(|e| AppError::Internal(format!("ONNX run: {e}")))?;
            let (_, data) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| AppError::Internal(format!("ONNX output: {e}")))?;
            let mut v: Vec<f32> = data.to_vec();
            l2_normalize(&mut v);
            Ok(v)
        }
    }

    fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
        if from == to || input.is_empty() {
            return input.to_vec();
        }
        let ratio = to as f64 / from as f64;
        let out_len = ((input.len() as f64) * ratio).round() as usize;
        (0..out_len)
            .map(|i| {
                let src = i as f64 / ratio;
                let idx = src.floor() as usize;
                let frac = (src - idx as f64) as f32;
                let a = input[idx.min(input.len() - 1)];
                let b = input[(idx + 1).min(input.len() - 1)];
                a + (b - a) * frac
            })
            .collect()
    }

    /// Center `want` samples (zero-padded if shorter).
    fn center_window(mono: &[f32], want: usize) -> Vec<f32> {
        if mono.len() <= want {
            let mut v = mono.to_vec();
            v.resize(want, 0.0);
            return v;
        }
        let start = (mono.len() - want) / 2;
        mono[start..start + want].to_vec()
    }

    fn l2_normalize(v: &mut [f32]) {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            for x in v.iter_mut() {
                *x /= n;
            }
        }
    }
}
