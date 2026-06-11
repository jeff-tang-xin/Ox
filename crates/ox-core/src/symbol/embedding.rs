use anyhow::Result;
use candle_core::{Device, Tensor, DType};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::Tokenizer;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::EmbeddingConfig;

/// Local embedding model using BERT (all-MiniLM-L6-v2, 384-dim).
pub struct EmbeddingModel {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl EmbeddingModel {
    /// Load the model using configuration.
    ///
    /// # Model Sources
    /// - `model_source = "modelscope"` (default): git clone from modelscope.cn (fast in China)
    /// - `model_source = "huggingface"`: Download from HuggingFace Hub (or mirror)
    /// - `model_source = "local"`: Load from `local_model_dir`
    pub fn with_config(config: &EmbeddingConfig) -> Result<Self> {
        let device = Device::Cpu;

        let cache_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ox")
            .join("models");

        std::fs::create_dir_all(&cache_dir)
            .map_err(|e| anyhow::anyhow!("Failed to create cache directory {:?}: {}", cache_dir, e))?;

        tracing::info!("[EMBEDDING] Using cache directory: {:?}", cache_dir);

        let (config_path, model_path, tokenizer_path) = match config.model_source.as_str() {
            "local" => Self::load_from_local(config)?,
            "modelscope" => Self::load_from_modelscope(config, &cache_dir)?,
            _ => Self::load_from_huggingface(config, &cache_dir)?,
        };

        // Load model config
        let config_parsed: Config = serde_json::from_reader(std::fs::File::open(&config_path)?)
            .map_err(|e| anyhow::anyhow!("Failed to parse config.json: {}", e))?;

        // Load model weights via memory-mapped safetensors
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[&model_path],
                DType::F32,
                &device,
            )?
        };
        let model = BertModel::load(vb, &config_parsed)?;

        // Load tokenizer
        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {e}"))?;

        tracing::info!("[EMBEDDING] Model loaded successfully");

        Ok(Self { model, tokenizer, device })
    }

    /// Load from local directory (model_source = "local").
    fn load_from_local(config: &EmbeddingConfig) -> Result<(PathBuf, PathBuf, PathBuf)> {
        let local_dir = PathBuf::from(&config.local_model_dir);
        if !local_dir.exists() {
            anyhow::bail!(
                "Local model directory does not exist: {:?}. \
                 Please download the model first or set model_source = \"modelscope\".",
                local_dir
            );
        }
        tracing::info!("[EMBEDDING] Loading model from local directory: {:?}", local_dir);
        Ok((
            local_dir.join("config.json"),
            local_dir.join("model.safetensors"),
            local_dir.join("tokenizer.json"),
        ))
    }

    /// Download from ModelScope via git clone (model_source = "modelscope").
    /// Clone URL: {modelscope_url}/{model_id}.git
    /// Target:    {cache_dir}/{model_id}/
    fn load_from_modelscope(config: &EmbeddingConfig, cache_dir: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
        // Derive local dir name from model_id (e.g. "sentence-transformers/all-MiniLM-L6-v2" → "all-MiniLM-L6-v2")
        let dir_name = config.model_id.rsplit('/').next().unwrap_or(&config.model_id);
        let model_dir = cache_dir.join(dir_name);

        if model_dir.join("model.safetensors").exists() {
            tracing::info!("[EMBEDDING] Model already cached at {:?}, skipping download", model_dir);
        } else {
            let clone_url = format!("{}/{}.git", config.modelscope_url.trim_end_matches('/'), config.model_id);
            tracing::info!("[EMBEDDING] Cloning {} → {:?} ...", clone_url, model_dir);

            // If partial clone exists, remove it first to avoid conflicts
            if model_dir.exists() {
                std::fs::remove_dir_all(&model_dir)
                    .map_err(|e| anyhow::anyhow!("Failed to clean partial clone {:?}: {}", model_dir, e))?;
            }

            let status = Command::new("git")
                .args(["clone", "--depth", "1", &clone_url])
                .arg(&model_dir)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to run git clone: {}. Is git installed?", e))?;

            if !status.success() {
                anyhow::bail!(
                    "git clone failed (exit code: {:?}). Check network or try model_source = \"local\".",
                    status.code()
                );
            }
            tracing::info!("[EMBEDDING] Clone complete");
        }

        Ok((
            model_dir.join("config.json"),
            model_dir.join("model.safetensors"),
            model_dir.join("tokenizer.json"),
        ))
    }

    /// Download from HuggingFace Hub or mirror via hf-hub API (model_source = "huggingface").
    fn load_from_huggingface(config: &EmbeddingConfig, cache_dir: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
        let endpoint = Self::resolve_endpoint(&config.hf_endpoint);
        tracing::info!("[EMBEDDING] Using HF endpoint: {}", endpoint);

        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_cache_dir(cache_dir.to_path_buf())
            .with_endpoint(endpoint)
            .with_progress(true)
            .build()?
            .repo(hf_hub::Repo::model(config.model_id.clone()));

        tracing::info!("[EMBEDDING] Downloading {} ({}-dim)...", config.model_id, config.dimension);

        let config_path = api.get("config.json")?;
        let model_path = api.get("model.safetensors")?;
        let tokenizer_path = api.get("tokenizer.json")?;
        Ok((config_path, model_path, tokenizer_path))
    }

    /// Resolve the HuggingFace endpoint with fallback chain:
    /// 1. Explicit config value
    /// 2. $HF_ENDPOINT environment variable
    /// 3. hf-mirror.com (China mirror, fast for CN users)
    fn resolve_endpoint(config_endpoint: &str) -> String {
        if !config_endpoint.is_empty() {
            return config_endpoint.to_string();
        }
        if let Ok(env_endpoint) = std::env::var("HF_ENDPOINT") {
            if !env_endpoint.is_empty() {
                return env_endpoint;
            }
        }
        // Default to China mirror (hf-mirror.com)
        // Set hf_endpoint = "https://huggingface.co" in config for official endpoint
        "https://hf-mirror.com".to_string()
    }

    /// Load the model with default settings (hf-mirror.com, all-MiniLM-L6-v2).
    pub fn new() -> Result<Self> {
        Self::with_config(&EmbeddingConfig::default())
    }

    /// Embed a single text into a dimensional f32 vector.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let tokens = self.tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("Tokenization error: {e}"))?;

        let input_ids = Tensor::new(tokens.get_ids(), &self.device)?.unsqueeze(0)?;
        let attention_mask = Tensor::new(
            tokens.get_attention_mask(),
            &self.device,
        )?.unsqueeze(0)?;

        let token_type_ids = Tensor::new(
            tokens.get_type_ids(),
            &self.device,
        )?.unsqueeze(0)?;

        let output = self.model.forward(
            &input_ids,
            &token_type_ids,
            Some(&attention_mask),
        )?;

        // Mean pooling: average all token embeddings (exclude padding)
        let hidden = output; // [1, seq_len, dim]
        let mask = attention_mask.unsqueeze(2)?.to_dtype(candle_core::DType::F32)?; // [1, seq_len, 1]
        let sum = hidden.broadcast_mul(&mask)?.sum(1)?; // [1, dim]
        let count = mask.sum(1)?.to_dtype(DType::F32)?.squeeze(1)?; // [1] → scalar-like
        // Use broadcast_div — candle ops are strict (no auto-broadcast)
        let pooled = sum.broadcast_div(&count.unsqueeze(1)?)?; // [1, dim]

        let embedding: Vec<f32> = pooled.squeeze(0)?.to_vec1()?;
        Ok(embedding)
    }

    /// Embed multiple texts in a true batch forward pass.
    /// All texts are tokenized, padded to the same length, and processed
    /// in a single model inference call — much faster than sequential embed().
    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if texts.len() == 1 {
            return Ok(vec![self.embed(texts[0])?]);
        }

        // 1. Tokenize all texts
        let encodings: Vec<_> = texts
            .iter()
            .map(|t| {
                self.tokenizer
                    .encode(*t, true)
                    .map_err(|e| anyhow::anyhow!("Tokenization error: {e}"))
            })
            .collect::<Result<Vec<_>>>()?;

        // 2. Find max sequence length for padding
        let max_len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(1);

        // 3. Build padded input_ids, attention_mask, token_type_ids tensors
        let mut all_ids: Vec<i64> = Vec::with_capacity(texts.len() * max_len);
        let mut all_mask: Vec<i64> = Vec::with_capacity(texts.len() * max_len);
        let mut all_types: Vec<i64> = Vec::with_capacity(texts.len() * max_len);

        for enc in &encodings {
            let ids = enc.get_ids();
            let mask = enc.get_attention_mask();
            let types = enc.get_type_ids();
            let pad_len = max_len - ids.len();

            all_ids.extend(ids.iter().map(|&v| v as i64));
            all_ids.extend(std::iter::repeat(0i64).take(pad_len));

            all_mask.extend(mask.iter().map(|&v| v as i64));
            all_mask.extend(std::iter::repeat(0i64).take(pad_len));

            all_types.extend(types.iter().map(|&v| v as i64));
            all_types.extend(std::iter::repeat(0i64).take(pad_len));
        }

        let batch = texts.len();
        let input_ids = Tensor::new(all_ids.as_slice(), &self.device)?.reshape((batch, max_len))?;
        let attention_mask = Tensor::new(all_mask.as_slice(), &self.device)?.reshape((batch, max_len))?;
        let token_type_ids = Tensor::new(all_types.as_slice(), &self.device)?.reshape((batch, max_len))?;

        // 4. Single batch forward pass
        let hidden = self.model.forward(
            &input_ids,
            &token_type_ids,
            Some(&attention_mask),
        )?; // [batch, seq_len, dim]

        // 5. Mean pooling per sequence (exclude padding via attention mask)
        let mask_f32 = attention_mask.unsqueeze(2)?.to_dtype(DType::F32)?; // [batch, seq_len, 1]
        let masked = hidden.broadcast_mul(&mask_f32)?; // [batch, seq_len, dim]
        let sum = masked.sum(1)?; // [batch, dim]
        let count = mask_f32.sum(1)?; // [batch, 1]
        // broadcast_div — candle ops are strict, no auto-broadcast
        let pooled = sum.broadcast_div(&count)?; // [batch, dim]

        // 6. Extract per-text embeddings
        let pooled_vec = pooled.to_vec2()?; // Vec<Vec<f32>>
        Ok(pooled_vec)
    }
}
