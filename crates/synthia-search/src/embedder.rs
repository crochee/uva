//! [`Embedder`]—pluggable text → dense-vector.
//!
//! [`HashingEmbedder`] is the default. It is fully deterministic
//! (FNV-1a 64-bit hash of each token, sign bit for ±1), zero
//! dependency, CPU-only. Production code that wants true
//! embeddings should wire `crate::ModelProviderEmbedder`
//! (feature `provider`) or its own `Embedder` impl.

use std::sync::Arc;

use crate::Tokenizer;

pub trait Embedder: Send + Sync + 'static {
    fn dim(&self) -> usize;
    fn embed(&self, text: &str) -> Vec<f32>;

    /// Embed a batch in one inner trip. The default
    /// implementation serialises through [`Embedder::embed`];
    /// embedders whose underlying engine already accepts a
    /// batch (e.g. `ModelProviderEmbedder` over a real API)
    /// override it so the batch driver only pays one wire
    /// trip per chunk. Returned `Vec<Vec<f32>>` MUST keep
    /// the input order and length.
    ///
    /// : added as a default-implemented
    /// method so the trait extension is non-breaking—every
    /// existing embedder that only implemented `embed` keeps
    /// compiling.
    fn embed_batch(&self, texts: Vec<String>) -> Vec<Vec<f32>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(&t));
        }
        out
    }
}

pub struct HashingEmbedder {
    dim: usize,
    tokenizer: Arc<dyn Tokenizer>,
}

impl HashingEmbedder {
    pub fn new(dim: usize, tokenizer: Arc<dyn Tokenizer>) -> Self {
        Self { dim, tokenizer }
    }
}

impl Embedder for HashingEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        for tok in self.tokenizer.tokenize(text) {
            let h = fnv1a(tok.as_bytes());
            let idx = (h as usize) % self.dim;
            let sign = if (h >> 63) & 1 == 0 { 1.0 } else { -1.0 };
            v[idx] += sign;
        }
        l2_normalize(&mut v);
        v
    }

    fn embed_batch(&self, texts: Vec<String>) -> Vec<Vec<f32>> {
        // HashingEmbedder is CPU-only—the batch path
        // does not open a wire trip, but exposing the
        // method explicitly keeps `BatchEmbedder`'s
        // driver uniform across providers.
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(&t));
        }
        out
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn l2_normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 1e-12 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CjkTokenizer;

    fn embedder() -> HashingEmbedder {
        HashingEmbedder::new(64, Arc::new(CjkTokenizer))
    }

    #[test]
    fn dim_returns_configured_size() {
        assert_eq!(embedder().dim(), 64);
    }

    #[test]
    fn embed_returns_l2_normalised_vector() {
        let v = embedder().embed("hello world");
        assert_eq!(v.len(), 64);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "expected unit vector, got norm={norm}"
        );
    }

    #[test]
    fn embed_is_deterministic() {
        let e = embedder();
        let a = e.embed("PDF extract");
        let b = e.embed("PDF extract");
        assert_eq!(a, b);
    }

    #[test]
    fn embed_different_texts_differ() {
        let e = embedder();
        let a = e.embed("PDF extract");
        let b = e.embed("weather forecast");
        assert_ne!(a, b);
    }

    #[test]
    fn empty_text_returns_zero_vector() {
        let v = embedder().embed("");
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn l2_normalize_handles_zero_vector() {
        let mut v = vec![0f32; 4];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0f32; 4]);
    }
}
