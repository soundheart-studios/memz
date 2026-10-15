//! Explicit local-only BGE inference. No network clients or model discovery.
pub mod relations;
use crate::{Error, Result, capture::digest};
use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path};
pub const DIM: usize = 384;
pub const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub backend: String,
    pub model: String,
    pub model_sha: String,
    pub tokenizer_sha: String,
    pub config_sha: String,
    pub dimensions: usize,
    pub pooling: String,
    pub normalization: String,
    pub metric: String,
    pub query_prefix: String,
    pub document_prefix: String,
    pub max_tokens: usize,
}
impl Identity {
    pub fn for_artifacts(model_sha: String, tokenizer_sha: String, config_sha: String) -> Self {
        Self {
            backend: "candle-0.11.0-cpu-f32/sqlite-vec-0.1.9".into(),
            model: "BAAI/bge-small-en-v1.5".into(),
            model_sha,
            tokenizer_sha,
            config_sha,
            dimensions: DIM,
            pooling: "cls".into(),
            normalization: "l2".into(),
            metric: "cosine".into(),
            query_prefix: QUERY_PREFIX.into(),
            document_prefix: String::new(),
            max_tokens: 512,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self
            != &Self::for_artifacts(
                self.model_sha.clone(),
                self.tokenizer_sha.clone(),
                self.config_sha.clone(),
            )
            || [&self.model_sha, &self.tokenizer_sha, &self.config_sha]
                .iter()
                .any(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(Error::new(
                "unknown_identity",
                "unsupported embedding identity",
            ));
        }
        Ok(())
    }
    pub fn generation(&self) -> Result<String> {
        self.validate()?;
        Ok(digest(
            &serde_json::to_vec(self).map_err(|e| Error::new("identity", e.to_string()))?,
        ))
    }
}
pub fn validate_vector(v: &[f32]) -> Result<()> {
    let norm = v.iter().map(|x| x * x).sum::<f32>();
    if v.len() != DIM || v.iter().any(|x| !x.is_finite()) || (norm - 1.).abs() > 0.002 {
        return Err(Error::new(
            "invalid_embedding",
            "expected normalized finite 384-dimensional vector",
        ));
    }
    Ok(())
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let f = std::fs::File::open(path).map_err(|_| {
        Error::new(
            "provider_unavailable",
            "configured local model artifact is missing",
        )
    })?;
    if !f.metadata()?.is_file() || f.metadata()?.len() > limit {
        return Err(Error::new(
            "invalid_model",
            "artifact exceeds model resource bounds",
        ));
    }
    let mut bytes = Vec::new();
    f.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::new(
            "invalid_model",
            "artifact exceeds model resource bounds",
        ));
    }
    Ok(bytes)
}
pub struct LocalModel {
    model: BertModel,
    tokenizer: tokenizers::Tokenizer,
    pub identity: Identity,
}
impl LocalModel {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(Error::new(
                "invalid_model",
                "model directory must be explicitly configured and absolute",
            ));
        }
        let config = read_bounded(&path.join("config.json"), 65536)?;
        let tokenizer = read_bounded(&path.join("tokenizer.json"), 8 * 1024 * 1024)?;
        let weights = read_bounded(&path.join("model.safetensors"), 160 * 1024 * 1024)?;
        let identity =
            Identity::for_artifacts(digest(&weights), digest(&tokenizer), digest(&config));
        let config: Config = serde_json::from_slice(&config)
            .map_err(|_| Error::new("invalid_model", "invalid BERT configuration"))?;
        if config.hidden_size != DIM
            || config.num_hidden_layers != 12
            || config.num_attention_heads != 12
            || config.intermediate_size != 1536
            || config.vocab_size != 30522
            || config.max_position_embeddings != 512
            || config.type_vocab_size != 2
        {
            return Err(Error::new(
                "invalid_model",
                "only BGE small English v1.5 architecture is qualified",
            ));
        }
        let mut tokenizer = tokenizers::Tokenizer::from_bytes(tokenizer)
            .map_err(|e| Error::new("invalid_model", e.to_string()))?;
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: 512,
                ..Default::default()
            }))
            .map_err(|e| Error::new("invalid_model", e.to_string()))?;
        let vb = VarBuilder::from_buffered_safetensors(weights, DType::F32, &Device::Cpu)
            .map_err(inference_error)?;
        let model = BertModel::load(vb, &config).map_err(inference_error)?;
        Ok(Self {
            model,
            tokenizer,
            identity,
        })
    }
    pub fn embed(&self, text: &str, query: bool) -> Result<Vec<f32>> {
        if text.is_empty() || text.len() > 65536 {
            return Err(Error::new("invalid_input", "embedding text outside bounds"));
        }
        let input = if query {
            format!("{QUERY_PREFIX}{text}")
        } else {
            text.to_owned()
        };
        let tokens = self
            .tokenizer
            .encode(input, true)
            .map_err(|e| Error::new("invalid_input", e.to_string()))?;
        let ids = Tensor::new(tokens.get_ids(), &Device::Cpu)
            .and_then(|t| t.unsqueeze(0))
            .map_err(inference_error)?;
        let types = ids.zeros_like().map_err(inference_error)?;
        let output = self
            .model
            .forward(&ids, &types, None)
            .and_then(|t| t.i((0, 0)))
            .and_then(|t| t.to_vec1::<f32>())
            .map_err(inference_error)?;
        let norm = output.iter().map(|x| x * x).sum::<f32>().sqrt();
        let output: Vec<f32> = output.into_iter().map(|x| x / norm).collect();
        validate_vector(&output)?;
        Ok(output)
    }
}
fn inference_error(e: candle_core::Error) -> Error {
    Error::new("provider_unavailable", e.to_string())
}

/// One model and one in-flight inference per daemon, including timed-out work.
type QueryCache = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<(String, Vec<f32>)>>>;
#[derive(Clone)]
pub struct Engine {
    model: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<LocalModel>>>>,
    pool: crate::jobs::WorkerPool,
    queries: QueryCache,
}
impl Engine {
    pub fn start(path: std::path::PathBuf) -> Result<Self> {
        let engine = Self {
            model: Default::default(),
            queries: Default::default(),
            pool: crate::jobs::WorkerPool::new(1, 512 * 1024 * 1024)?,
        };
        let slot = engine.model.clone();
        // Loading is bounded and asynchronous; a failed load remains unavailable until restart.
        let _ = engine.pool.start(512 * 1024 * 1024, move || {
            if let Ok(model) = LocalModel::load(&path) {
                *slot.lock().unwrap() = Some(std::sync::Arc::new(model));
            }
        })?;
        Ok(engine)
    }
    pub fn identity(&self) -> Option<Identity> {
        self.model
            .lock()
            .unwrap()
            .as_ref()
            .map(|m| m.identity.clone())
    }
    pub fn active(&self) -> usize {
        self.pool.active()
    }
    pub fn embed(
        &self,
        text: String,
        query: bool,
        timeout: std::time::Duration,
    ) -> Result<Vec<f32>> {
        if text.len() > 65536 {
            return Err(Error::new("invalid_input", "embedding text outside bounds"));
        }
        let model = self.model.lock().unwrap().clone().ok_or_else(|| {
            Error::new("provider_unavailable", "local model is cold or unavailable")
        })?;
        self.pool
            .start(512 * 1024 * 1024, move || model.embed(&text, query))?
            .wait(timeout)?
    }
    pub fn query(&self, text: String, timeout: std::time::Duration) -> Result<Vec<f32>> {
        if text.is_empty() || text.len() > 2048 {
            return Err(Error::new(
                "invalid_query",
                "query outside embedding bounds",
            ));
        }
        let model = self.model.lock().unwrap().clone().ok_or_else(|| {
            Error::new("semantic_unavailable", "local provider cold or unavailable")
        })?;
        let key = digest(
            serde_json::json!([model.identity, text])
                .to_string()
                .as_bytes(),
        );
        if let Some((_, v)) = self.queries.lock().unwrap().iter().find(|(k, _)| k == &key) {
            return Ok(v.clone());
        }
        let cache = self.queries.clone();
        self.pool
            .start(512 * 1024 * 1024, move || {
                let v = model.embed(&text, true)?;
                let mut cache = cache.lock().unwrap();
                if cache.len() >= 64 {
                    cache.pop_front();
                }
                cache.push_back((key, v.clone()));
                Ok(v)
            })?
            .wait(timeout)?
    }
}
