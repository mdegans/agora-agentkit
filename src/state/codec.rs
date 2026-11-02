//! Brotli: keyframes on their own, deltas with a keyframe's plaintext as
//! the custom dictionary

use brotli::enc::BrotliEncoderParams;

use crate::govlog::canonical_json;

/// Brotli quality
pub const QUALITY: i32 = 11;
/// Brotli window, log2: 16 MiB, so a delta reaches its whole dictionary
pub const LGWIN: i32 = 24;

/// A delta larger than this fraction of a fresh keyframe of the same
/// version starts a new keyframe (step 0, 2026-11-02: states grow a lot,
/// so the old keyframe's size is the wrong yardstick)
pub const DEFAULT_MAX_DELTA_RATIO: f64 = 0.5;
/// Deltas against one keyframe, at most
pub const DEFAULT_MAX_CHAIN: u32 = 64;

/// A blob that wouldn't decompress
#[derive(Debug, thiserror::Error)]
#[error("brotli: {0}")]
pub struct CodecError(#[from] std::io::Error);

/// What is sealed for a state: the canonical bytes of `FsStorage`'s
/// envelope, `{format, state}`
pub fn plaintext(state: &serde_json::Value) -> Vec<u8> {
    canonical_json(&serde_json::json!({
        "format": crate::reactor::storage::FORMAT,
        "state": state,
    }))
}

fn params() -> BrotliEncoderParams {
    BrotliEncoderParams {
        quality: QUALITY,
        lgwin: LGWIN,
        ..Default::default()
    }
}

/// Compress `data`, against `dict` if given
pub fn compress(data: &[u8], dict: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut input = [0u8; 4096];
    let mut output = [0u8; 4096];
    let mut noop = |_: &mut brotli::interface::PredictionModeContextMap<
        brotli::InputReferenceMut,
    >,
                    _: &mut [brotli::interface::StaticCommand],
                    _: brotli::InputPair,
                    _: &mut brotli::enc::StandardAlloc| ();
    brotli::enc::BrotliCompressCustomIoCustomDict(
        &mut brotli::IoReaderWrapper(&mut &data[..]),
        &mut brotli::IoWriterWrapper(&mut out),
        &mut input,
        &mut output,
        &params(),
        brotli::enc::StandardAlloc::default(),
        &mut noop,
        dict.unwrap_or(&[]),
        std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof"),
    )
    .expect("compressing in memory doesn't fail");
    out
}

/// Decompress `data`, against the `dict` it was compressed with
pub fn decompress(
    data: &[u8],
    dict: Option<&[u8]>,
) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    brotli_decompressor::BrotliDecompressCustomDict(
        &mut &data[..],
        &mut out,
        &mut [0u8; 4096],
        &mut [0u8; 4096],
        dict.map(<[u8]>::to_vec).unwrap_or_default(),
    )?;
    Ok(out)
}

/// When a version starts a new keyframe
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyframePolicy {
    /// See [`DEFAULT_MAX_DELTA_RATIO`]
    pub max_delta_ratio: f64,
    /// See [`DEFAULT_MAX_CHAIN`]
    pub max_chain: u32,
}

impl Default for KeyframePolicy {
    fn default() -> Self {
        Self {
            max_delta_ratio: DEFAULT_MAX_DELTA_RATIO,
            max_chain: DEFAULT_MAX_CHAIN,
        }
    }
}

/// The current keyframe, to encode against
#[derive(Debug, Clone, Copy)]
pub struct Keyframe<'a> {
    pub plaintext: &'a [u8],
    /// Deltas already written against it
    pub deltas: u32,
}

/// A compressed version
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Encoded {
    Keyframe(Vec<u8>),
    Delta(Vec<u8>),
}

impl KeyframePolicy {
    /// Encode `plaintext`: a delta against `keyframe`, unless there is
    /// none, its chain is full, or the delta is over
    /// [`max_delta_ratio`](Self::max_delta_ratio) of a fresh keyframe
    pub fn encode(
        &self,
        plaintext: &[u8],
        keyframe: Option<Keyframe<'_>>,
    ) -> Encoded {
        let fresh = || Encoded::Keyframe(compress(plaintext, None));
        let Some(kf) = keyframe else {
            return fresh();
        };
        if kf.deltas >= self.max_chain {
            return fresh();
        }
        let delta = compress(plaintext, Some(kf.plaintext));
        let keyframe = compress(plaintext, None);
        if delta.len() as f64 > self.max_delta_ratio * keyframe.len() as f64 {
            Encoded::Keyframe(keyframe)
        } else {
            Encoded::Delta(delta)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(n: usize, extra: &str) -> Vec<u8> {
        let memory: String = (0..n)
            .map(|i| format!("Day {i}: I read a thread about governance. "))
            .collect();
        plaintext(&serde_json::json!({ "memory": memory + extra, "n": n }))
    }

    #[test]
    fn keyframes_and_deltas_round_trip() {
        let a = state(300, "");
        let b = state(300, "And one more thing.");
        let kf = compress(&a, None);
        assert_eq!(decompress(&kf, None).unwrap(), a);
        let delta = compress(&b, Some(&a));
        assert!(
            delta.len() < kf.len() / 10,
            "{} vs {}",
            delta.len(),
            kf.len()
        );
        assert_eq!(decompress(&delta, Some(&a)).unwrap(), b);
    }

    #[test]
    fn a_delta_against_the_wrong_base_does_not_open() {
        let a = state(300, "");
        let b = state(300, "x");
        let other = state(10, "unrelated");
        let delta = compress(&b, Some(&a));
        assert_ne!(decompress(&delta, Some(&other)).ok(), Some(b.clone()));
        assert_ne!(decompress(&delta, None).ok(), Some(b));
    }

    #[test]
    fn the_policy_picks_keyframes_by_fresh_size_and_chain() {
        let policy = KeyframePolicy::default();
        let a = state(300, "");
        let b = state(300, "small change");
        assert!(matches!(policy.encode(&b, None), Encoded::Keyframe(_)));
        let kf = Keyframe {
            plaintext: &a,
            deltas: 0,
        };
        assert!(matches!(policy.encode(&b, Some(kf)), Encoded::Delta(_)));
        let full = Keyframe {
            plaintext: &a,
            deltas: DEFAULT_MAX_CHAIN,
        };
        assert!(matches!(
            policy.encode(&b, Some(full)),
            Encoded::Keyframe(_)
        ));
        // Grown past its keyframe: the delta is most of a fresh keyframe
        let grown = state(3000, "");
        let tiny = state(1, "");
        let kf = Keyframe {
            plaintext: &tiny,
            deltas: 0,
        };
        assert!(matches!(
            policy.encode(&grown, Some(kf)),
            Encoded::Keyframe(_)
        ));
    }
}
