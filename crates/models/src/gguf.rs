//! Minimal GGUF header reader.
//!
//! We only read the metadata key/value section (not tensors). Large arrays
//! (tokenizer vocabularies) are skipped without being materialised, so
//! inspecting a multi-gigabyte model reads only a few megabytes.

use std::collections::BTreeMap;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

const MAGIC: u32 = 0x4655_4747; // "GGUF" little-endian
const MAX_STRING: u64 = 64 * 1024 * 1024;
const KEEP_ARRAY_MAX: u64 = 1024;

#[derive(Debug, thiserror::Error)]
pub enum GgufError {
    #[error("not a GGUF file (bad magic)")]
    BadMagic,
    #[error("unsupported GGUF version {0}")]
    Version(u32),
    #[error("corrupt GGUF header: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U64(u64),
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(String),
    /// Small arrays are kept; large ones only record their length.
    Array {
        len: u64,
        items: Vec<Value>,
    },
}

impl Value {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::U64(v) => Some(*v),
            Value::I64(v) if *v >= 0 => Some(*v as u64),
            Value::F64(v) if *v >= 0.0 => Some(*v as u64),
            // Per-layer arrays (e.g. head_count_kv): use the maximum.
            Value::Array { items, .. } => items.iter().filter_map(Value::as_u64).max(),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GgufInfo {
    pub version: u32,
    pub tensor_count: u64,
    pub metadata: BTreeMap<String, Value>,
}

impl GgufInfo {
    pub fn read_path(path: &Path) -> Result<Self, GgufError> {
        let f = std::fs::File::open(path)?;
        Self::read(BufReader::with_capacity(256 * 1024, f))
    }

    pub fn read<R: Read + Seek>(mut r: R) -> Result<Self, GgufError> {
        if read_u32(&mut r)? != MAGIC {
            return Err(GgufError::BadMagic);
        }
        let version = read_u32(&mut r)?;
        if !(2..=3).contains(&version) {
            return Err(GgufError::Version(version));
        }
        let tensor_count = read_u64(&mut r)?;
        let kv_count = read_u64(&mut r)?;
        if kv_count > 1_000_000 {
            return Err(GgufError::Corrupt(format!("{kv_count} metadata entries")));
        }
        let mut metadata = BTreeMap::new();
        for _ in 0..kv_count {
            let key = read_string(&mut r)?;
            let ty = read_u32(&mut r)?;
            let value = read_value(&mut r, ty, true)?;
            if let Some(v) = value {
                metadata.insert(key, v);
            }
        }
        Ok(Self { version, tensor_count, metadata })
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.metadata.get(key)
    }
    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }
    pub fn architecture(&self) -> Option<&str> {
        self.str("general.architecture")
    }
    fn arch_u64(&self, suffix: &str) -> Option<u64> {
        let arch = self.architecture()?;
        self.get(&format!("{arch}.{suffix}")).and_then(Value::as_u64)
    }
    pub fn name(&self) -> Option<&str> {
        self.str("general.name")
    }
    pub fn size_label(&self) -> Option<&str> {
        self.str("general.size_label")
    }
    pub fn license(&self) -> Option<&str> {
        self.str("general.license")
    }
    pub fn context_length(&self) -> Option<u64> {
        self.arch_u64("context_length")
    }
    pub fn block_count(&self) -> Option<u64> {
        self.arch_u64("block_count")
    }
    pub fn embedding_length(&self) -> Option<u64> {
        self.arch_u64("embedding_length")
    }
    pub fn head_count(&self) -> Option<u64> {
        self.arch_u64("attention.head_count")
    }
    pub fn head_count_kv(&self) -> Option<u64> {
        self.arch_u64("attention.head_count_kv").or_else(|| self.head_count())
    }
    pub fn key_length(&self) -> Option<u64> {
        self.arch_u64("attention.key_length")
    }
    pub fn value_length(&self) -> Option<u64> {
        self.arch_u64("attention.value_length")
    }
    pub fn chat_template(&self) -> Option<&str> {
        self.str("tokenizer.chat_template")
    }
    pub fn file_type(&self) -> Option<u64> {
        self.get("general.file_type").and_then(Value::as_u64)
    }
    pub fn quantization(&self) -> String {
        self.file_type().map(ftype_name).unwrap_or("unknown").to_string()
    }
    pub fn is_vision_projector(&self) -> bool {
        self.architecture() == Some("clip")
    }
}

/// llama.cpp `llama_ftype` names.
pub fn ftype_name(t: u64) -> &'static str {
    match t {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        7 => "Q8_0",
        8 => "Q5_0",
        9 => "Q5_1",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        19 => "IQ2_XXS",
        20 => "IQ2_XS",
        21 => "Q2_K_S",
        22 => "IQ3_XS",
        23 => "IQ3_XXS",
        24 => "IQ1_S",
        25 => "IQ4_NL",
        26 => "IQ3_S",
        27 => "IQ3_M",
        28 => "IQ2_S",
        29 => "IQ2_M",
        30 => "IQ4_XS",
        31 => "IQ1_M",
        32 => "BF16",
        36 => "TQ1_0",
        37 => "TQ2_0",
        38 => "MXFP4",
        _ => "unknown",
    }
}

/// Cheap check: does the file start with the GGUF magic?
pub fn is_gguf(path: &Path) -> bool {
    let mut buf = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut buf)).is_ok() && u32::from_le_bytes(buf) == MAGIC
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, GgufError> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn read_u64<R: Read>(r: &mut R) -> Result<u64, GgufError> {
    let mut b = [0; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn read_string<R: Read>(r: &mut R) -> Result<String, GgufError> {
    let len = read_u64(r)?;
    if len > MAX_STRING {
        return Err(GgufError::Corrupt(format!("string of {len} bytes")));
    }
    let mut buf = vec![0; len as usize];
    r.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}
fn skip_string<R: Read + Seek>(r: &mut R) -> Result<(), GgufError> {
    let len = read_u64(r)?;
    if len > MAX_STRING {
        return Err(GgufError::Corrupt(format!("string of {len} bytes")));
    }
    skip_bytes(r, len)
}

/// Skip forward. Small skips read through the buffer (a `seek` on a
/// `BufReader` would discard it, turning a vocab scan into thousands of syscalls).
fn skip_bytes<R: Read + Seek>(r: &mut R, n: u64) -> Result<(), GgufError> {
    if n <= 64 * 1024 {
        let copied = std::io::copy(&mut r.by_ref().take(n), &mut std::io::sink())?;
        if copied != n {
            return Err(GgufError::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
    } else {
        r.seek(SeekFrom::Current(n as i64))?;
    }
    Ok(())
}

fn scalar_size(ty: u32) -> Option<u64> {
    Some(match ty {
        0 | 1 | 7 => 1,
        2 | 3 => 2,
        4..=6 => 4,
        10..=12 => 8,
        _ => return None,
    })
}

fn read_value<R: Read + Seek>(r: &mut R, ty: u32, keep: bool) -> Result<Option<Value>, GgufError> {
    macro_rules! num {
        ($n:literal, $t:ty) => {{
            let mut b = [0u8; $n];
            r.read_exact(&mut b)?;
            <$t>::from_le_bytes(b)
        }};
    }
    let v = match ty {
        0 => Value::U64(num!(1, u8) as u64),
        1 => Value::I64(num!(1, i8) as i64),
        2 => Value::U64(num!(2, u16) as u64),
        3 => Value::I64(num!(2, i16) as i64),
        4 => Value::U64(num!(4, u32) as u64),
        5 => Value::I64(num!(4, i32) as i64),
        6 => Value::F64(num!(4, f32) as f64),
        7 => Value::Bool(num!(1, u8) != 0),
        8 => {
            if keep {
                Value::Str(read_string(r)?)
            } else {
                skip_string(r)?;
                return Ok(None);
            }
        }
        9 => {
            let item_ty = read_u32(r)?;
            let len = read_u64(r)?;
            let keep_items = keep && len <= KEEP_ARRAY_MAX && item_ty != 9;
            if !keep_items {
                // Skip quickly: fixed-size scalars by seeking, strings one by one.
                if let Some(sz) = scalar_size(item_ty) {
                    let bytes = sz.checked_mul(len).ok_or_else(|| GgufError::Corrupt("array overflow".into()))?;
                    skip_bytes(r, bytes)?;
                } else {
                    for _ in 0..len {
                        read_value(r, item_ty, false)?;
                    }
                }
                return Ok(Some(Value::Array { len, items: Vec::new() }));
            }
            let mut items = Vec::with_capacity(len as usize);
            for _ in 0..len {
                if let Some(v) = read_value(r, item_ty, true)? {
                    items.push(v);
                }
            }
            Value::Array { len, items }
        }
        10 => Value::U64(num!(8, u64)),
        11 => Value::I64(num!(8, i64)),
        12 => Value::F64(num!(8, f64)),
        other => return Err(GgufError::Corrupt(format!("unknown value type {other}"))),
    };
    Ok(Some(v))
}

/// Test helper: write a synthetic GGUF header.
#[doc(hidden)]
pub mod testutil {
    pub enum W<'a> {
        U32(u32),
        Str(&'a str),
        StrArray(&'a [&'a str]),
        I32Array(&'a [i32]),
    }
    pub fn build(kvs: &[(&str, W)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(0x4655_4747u32.to_le_bytes());
        out.extend(3u32.to_le_bytes());
        out.extend(0u64.to_le_bytes());
        out.extend((kvs.len() as u64).to_le_bytes());
        let s = |out: &mut Vec<u8>, s: &str| {
            out.extend((s.len() as u64).to_le_bytes());
            out.extend(s.as_bytes());
        };
        for (k, v) in kvs {
            s(&mut out, k);
            match v {
                W::U32(n) => {
                    out.extend(4u32.to_le_bytes());
                    out.extend(n.to_le_bytes());
                }
                W::Str(v) => {
                    out.extend(8u32.to_le_bytes());
                    s(&mut out, v);
                }
                W::StrArray(items) => {
                    out.extend(9u32.to_le_bytes());
                    out.extend(8u32.to_le_bytes());
                    out.extend((items.len() as u64).to_le_bytes());
                    for i in *items {
                        s(&mut out, i);
                    }
                }
                W::I32Array(items) => {
                    out.extend(9u32.to_le_bytes());
                    out.extend(5u32.to_le_bytes());
                    out.extend((items.len() as u64).to_le_bytes());
                    for i in *items {
                        out.extend(i.to_le_bytes());
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{W, build};
    use super::*;
    use std::io::Cursor;

    fn sample() -> Vec<u8> {
        let vocab: Vec<String> = (0..5000).map(|i| format!("tok{i}")).collect();
        let vocab_refs: Vec<&str> = vocab.iter().map(String::as_str).collect();
        build(&[
            ("general.architecture", W::Str("llama")),
            ("general.name", W::Str("Tiny Llama")),
            ("general.file_type", W::U32(15)),
            ("llama.context_length", W::U32(4096)),
            ("llama.block_count", W::U32(16)),
            ("llama.embedding_length", W::U32(2048)),
            ("llama.attention.head_count", W::U32(32)),
            ("llama.attention.head_count_kv", W::I32Array(&[8, 8, 4])),
            ("tokenizer.ggml.tokens", W::StrArray(&vocab_refs)),
            ("tokenizer.chat_template", W::Str("{% if tools %}...{% endif %}")),
        ])
    }

    #[test]
    fn parses_metadata_and_skips_large_arrays() {
        let info = GgufInfo::read(Cursor::new(sample())).unwrap();
        assert_eq!(info.version, 3);
        assert_eq!(info.architecture(), Some("llama"));
        assert_eq!(info.name(), Some("Tiny Llama"));
        assert_eq!(info.quantization(), "Q4_K_M");
        assert_eq!(info.context_length(), Some(4096));
        assert_eq!(info.block_count(), Some(16));
        assert_eq!(info.head_count_kv(), Some(8));
        match info.get("tokenizer.ggml.tokens").unwrap() {
            Value::Array { len, items } => {
                assert_eq!(*len, 5000);
                assert!(items.is_empty());
            }
            v => panic!("{v:?}"),
        }
        assert!(info.chat_template().unwrap().contains("tools"));
    }

    #[test]
    fn rejects_non_gguf() {
        assert!(matches!(GgufInfo::read(Cursor::new(b"PK\x03\x04aaaaaaaaaaaaaaaaaaaaaaaa".to_vec())), Err(GgufError::BadMagic)));
    }

    #[test]
    fn truncated_file_is_an_error_not_a_panic() {
        let mut data = sample();
        data.truncate(200);
        assert!(GgufInfo::read(Cursor::new(data)).is_err());
    }

    #[test]
    fn absurd_lengths_are_rejected() {
        let mut out = Vec::new();
        out.extend(0x4655_4747u32.to_le_bytes());
        out.extend(3u32.to_le_bytes());
        out.extend(0u64.to_le_bytes());
        out.extend(1u64.to_le_bytes());
        out.extend(u64::MAX.to_le_bytes());
        assert!(matches!(GgufInfo::read(Cursor::new(out)), Err(GgufError::Corrupt(_))));
    }
}
