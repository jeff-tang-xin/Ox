//! BGE (BAAI General Embedding) model support using Candle.
//!
//! Supports loading BGE models from local files (safetensors format).
//! Uses candle-core for tensor operations and tokenizers for tokenization.

use anyhow::Result;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use safetensors::tensor::{Dtype as SafetensorDType, SafeTensors};
use std::collections::HashMap;
use std::path::Path;
use tokenizers::Tokenizer;

/// Load tensor data from safetensors into a candle Tensor.
/// Handles F16->F32 and I64->I64 conversion as needed.
/// Skips unsupported dtypes (log at debug level).
fn load_tensor(tensors: &SafeTensors, name: &str, device: &Device) -> Option<Tensor> {
    let tensor_view = tensors.tensor(name).ok()?;
    let shape: Vec<usize> = tensor_view.shape().to_vec();
    let s_dtype = tensor_view.dtype();

    match s_dtype {
        SafetensorDType::F32 => {
            let data: Vec<f32> = tensor_view.data().chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect();
            Tensor::from_slice(&data, shape, device).ok()
        }
        SafetensorDType::F16 => {
            let data: Vec<f32> = tensor_view.data().chunks_exact(2)
                .map(|chunk| {
                    let bits = u16::from_le_bytes([chunk[0], chunk[1]]);
                    half::f16::from_bits(bits).to_f32()
                })
                .collect();
            Tensor::from_slice(&data, shape, device).ok()
        }
        SafetensorDType::I64 => {
            // I64 tensors (like position_ids) - load as i64 and convert to appropriate type
            let data: Vec<i64> = tensor_view.data().chunks_exact(8)
                .map(|chunk| i64::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7]]))
                .collect();
            // Convert to u32 for use as indices, then to tensor
            let data_u32: Vec<u32> = data.iter().map(|&x| x as u32).collect();
            Tensor::from_slice(&data_u32, shape, device).ok()
        }
        _ => {
            tracing::debug!("Skipping tensor {} with unsupported dtype {:?}", name, s_dtype);
            None
        }
    }
}

/// BGE Embedder for generating sentence embeddings using Candle.
pub struct BgeEmbedder {
    device: Device,
    model: BertModel,
    tokenizer: Tokenizer,
    hidden_size: usize,
    max_position_embeddings: usize,
}

impl BgeEmbedder {
    /// Load a BGE embedder from a model directory.
    ///
    /// The directory should contain:
    /// - model.safetensors: Model weights in safetensors format
    /// - tokenizer.json: Tokenizer files
    /// - config.json: Model configuration
    pub fn load(model_path: &Path) -> Result<Self> {
        let device = Device::Cpu;

        // Load tokenizer
        let tokenizer = Tokenizer::from_file(model_path.join("tokenizer.json"))
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

        // Load config
        let config_path = model_path.join("config.json");
        let config_content = std::fs::read_to_string(&config_path)?;
        let json_config: serde_json::Value = serde_json::from_str(&config_content)?;

        let hidden_size = json_config["hidden_size"].as_u64().unwrap_or(512) as usize;
        let vocab_size = json_config["vocab_size"].as_u64().unwrap_or(30522) as usize;
        let num_hidden_layers = json_config["num_hidden_layers"].as_u64().unwrap_or(12) as usize;
        let num_attention_heads = json_config["num_attention_heads"].as_u64().unwrap_or(12) as usize;
        let intermediate_size = json_config["intermediate_size"].as_u64().unwrap_or(3072) as usize;
        let max_position_embeddings = json_config["max_position_embeddings"]
            .as_u64()
            .unwrap_or(512) as usize;
        let type_vocab_size = json_config["type_vocab_size"].as_u64().unwrap_or(2) as usize;

        // Build BERT config for candle
        let config = Config {
            vocab_size,
            hidden_size,
            num_hidden_layers,
            num_attention_heads,
            intermediate_size,
            max_position_embeddings,
            type_vocab_size,
            ..Default::default()
        };

        // Load model weights from safetensors
        let model_file = model_path.join("model.safetensors");
        let buffer = std::fs::read(&model_file)?;
        let tensors = SafeTensors::deserialize(&buffer)?;

        // Convert tensors to HashMap, handling F16/F32/I64 conversion
        let mut tensor_map = HashMap::new();
        for (name, _view) in tensors.tensors() {
            if let Some(tensor) = load_tensor(&tensors, &name, &device) {
                tensor_map.insert(name.clone(), tensor);
            }
        }

        // Create VarBuilder from tensors
        let vb = VarBuilder::from_tensors(tensor_map, DType::F32, &device);

        // Build the BERT model
        let model = BertModel::load(vb, &config)?;

        Ok(Self {
            device,
            model,
            tokenizer,
            hidden_size,
            max_position_embeddings,
        })
    }

    /// Get the tokenizer for chunking purposes.
    pub fn tokenizer(&self) -> &Tokenizer {
        &self.tokenizer
    }

    /// Get the maximum sequence length supported by this model.
    pub fn max_position_embeddings(&self) -> usize {
        self.max_position_embeddings
    }

    /// Encode a single text into an embedding vector.
    ///
    /// Returns a normalized L2 embedding vector.
    pub fn encode(&self, text: &str) -> Result<Vec<f32>> {
        let encoding = self.tokenizer.encode(text, false)
            .map_err(|e| anyhow::anyhow!("Failed to tokenize: {}", e))?;

        let input_ids = encoding.get_ids();
        let attention_mask = encoding.get_attention_mask();
        let seq_len = input_ids.len();

        // Warn if sequence is too long for model
        if seq_len > self.max_position_embeddings {
            tracing::warn!(
                "Sequence length {} exceeds max_position_embeddings {}, truncating",
                seq_len,
                self.max_position_embeddings
            );
        }

        // Create tensors (truncate if necessary)
        let input_len = seq_len.min(self.max_position_embeddings);
        let input_ids = Tensor::new(input_ids[..input_len].to_vec(), &self.device)?
            .unsqueeze(0)?;
        let attention_mask = Tensor::new(attention_mask[..input_len].to_vec(), &self.device)?
            .unsqueeze(0)?;

        // Forward pass through BERT (3 args: input_ids, attention_mask, token_type_ids)
        let output = self.model.forward(&input_ids, &attention_mask, None)?;

        // Mean Pooling: [1, seq_len, hidden] -> [1, 512]
        let mean_pooled = output.mean(1)?;

        // L2 normalize: [1, 512] -> [1, 512]
        let norm = mean_pooled.sqr()?.sum_keepdim(1)?.sqrt()?;
        let normalized = mean_pooled.broadcast_div(&norm)?;

        // Squeeze to [512] then to Vec<f32>
        let vector = normalized.squeeze(0)?.to_vec1()?;

        Ok(vector)
    }

    /// Encode multiple texts into embedding vectors.
    pub fn encode_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.encode(t)).collect()
    }

    /// Get the embedding dimension.
    pub fn embedding_dim(&self) -> usize {
        self.hidden_size
    }
}

/// Compute cosine similarity between two vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm_a * norm_b + 1e-12)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_model_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ox/models/bge-small-zh-v1.5")
    }

    #[test]
    #[ignore] // Requires model files to be present
    fn test_bge_load() {
        let path = test_model_path();
        if !path.exists() {
            println!("Skipping test: model not found at {:?}", path);
            return;
        }

        let embedder = BgeEmbedder::load(&path);
        assert!(embedder.is_ok(), "Failed to load BGE model");

        let embedder = embedder.unwrap();
        assert_eq!(embedder.embedding_dim(), 512);
    }

    #[test]
    #[ignore] // Requires model files to be present
    fn test_bge_encode() {
        let path = test_model_path();
        if !path.exists() {
            println!("Skipping test: model not found at {:?}", path);
            return;
        }

        let embedder = BgeEmbedder::load(&path).unwrap();
        let embedding = embedder.encode("你好世界");

        assert!(embedding.is_ok());
        let embedding = embedding.unwrap();
        assert_eq!(embedding.len(), 512);

        // L2 norm should be approximately 1.0
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_cosine_similarity() {
        // Identical vectors
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);

        // Orthogonal vectors
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 0.001);

        // Opposite vectors
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 0.001);
    }
}
