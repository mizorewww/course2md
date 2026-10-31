//! Read-only cache evidence. Finding files is deliberately distinct from loading a model.
use crate::config::AsrProvider;
use crate::execution::hex;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheState {
    Missing,
    Partial,
    Cached,
    Loaded,
    Unsupported,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachePart {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub missing: Vec<String>,
    /// 一次遍历得到的 (相对路径, 字节数, 修改时间纳秒)，供指纹复用。
    #[serde(skip)]
    files: Vec<(String, u64, u128)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalModelStatus {
    pub provider: AsrProvider,
    pub model: String,
    pub state: CacheState,
    pub parts: Vec<CachePart>,
    pub bytes: u64,
    pub can_prepare: bool,
    pub last_error: Option<String>,
    pub loaded_at: Option<u64>,
    pub fingerprint: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    fingerprint: String,
    loaded_at: Option<u64>,
    last_error: Option<String>,
    version: String,
}

pub fn inspect(provider: AsrProvider, model: &str, root: &Path) -> Result<LocalModelStatus> {
    let home = crate::config::home_dir().unwrap_or_else(crate::config::cache_dir);
    let var = |keys: [&str; 2]| {
        keys.into_iter()
            .find_map(|key| std::env::var_os(key).filter(|value| !value.is_empty()))
            .map(PathBuf::from)
    };
    let apple_base = var(["QWEN3_CACHE_DIR", "QWEN3_ASR_CACHE_DIR"])
        .unwrap_or_else(|| home.join("Library/Caches"))
        .join("qwen3-speech");
    let hf_base = var(["HF_HUB_CACHE", "HUGGINGFACE_HUB_CACHE"]).unwrap_or_else(|| {
        std::env::var_os("HF_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache").join("huggingface"))
            .join("hub")
    });
    Ok(with_receipt(inspect_at(
        provider,
        model,
        root,
        &apple_base,
        &hf_base,
    )?))
}

/// 合并上次加载验证的结果（指纹与版本一致时才把 Cached 提升为 Loaded）。
pub fn with_receipt(mut status: LocalModelStatus) -> LocalModelStatus {
    if let Ok(bytes) = fs::read(receipt_path(&status))
        && let Ok(receipt) = serde_json::from_slice::<Receipt>(&bytes)
    {
        status.last_error = receipt.last_error;
        if status.state == CacheState::Cached
            && receipt.fingerprint == status.fingerprint
            && receipt.version == env!("CARGO_PKG_VERSION")
        {
            status.loaded_at = receipt.loaded_at;
            if receipt.loaded_at.is_some() {
                status.state = CacheState::Loaded;
            }
        }
    }
    status
}

/// One Apple model cache repository and the files the loaders require.
#[derive(Clone, Copy)]
pub(crate) struct ApplePart {
    pub repo: &'static str,
    pub files: &'static [&'static str],
    pub bundles: &'static [&'static str],
    /// The loaders also require safetensors shards referenced by an index.
    pub weights: bool,
}

const QWEN_17: &[ApplePart] = &[ApplePart {
    repo: "aufklarer/Qwen3-ASR-1.7B-MLX-8bit",
    files: &[
        "config.json",
        "vocab.json",
        "merges.txt",
        "tokenizer_config.json",
    ],
    bundles: &[],
    weights: true,
}];
const QWEN_06: &[ApplePart] = &[
    ApplePart {
        repo: "aufklarer/Qwen3-ASR-CoreML",
        files: &["config.json"],
        bundles: &[
            "encoder.mlmodelc",
            "embedding.mlmodelc",
            "decoder_part1.mlmodelc",
            "decoder_part2.mlmodelc",
        ],
        weights: false,
    },
    ApplePart {
        repo: "aufklarer/Qwen3-ASR-0.6B-MLX-4bit",
        files: &["vocab.json", "merges.txt", "tokenizer_config.json"],
        bundles: &[],
        weights: true,
    },
];
const WHISPER: &[ApplePart] = &[ApplePart {
    repo: "aufklarer/Whisper-Large-v3-Turbo-CoreML",
    files: &[
        "generation_config.json",
        "manifest.json",
        "tokenizer.json",
        "tokenizer_config.json",
        "vocab.json",
        "merges.txt",
    ],
    bundles: &[
        "MelSpectrogram.mlmodelc",
        "AudioEncoder.mlmodelc",
        "TextDecoder.mlmodelc",
        "TextDecoderContextPrefill.mlmodelc",
    ],
    weights: false,
}];
pub(crate) const APPLE_VAD: ApplePart = ApplePart {
    repo: "aufklarer/Silero-VAD-v6.2.1-CoreML",
    files: &["config.json"],
    bundles: &["silero_vad.mlmodelc"],
    weights: false,
};

pub(crate) fn apple_parts(model: &str) -> Option<&'static [ApplePart]> {
    match model {
        "qwen3-1.7b" => Some(QWEN_17),
        "qwen3-0.6b" => Some(QWEN_06),
        "whisper" => Some(WHISPER),
        _ => None,
    }
}

/// Explicit roots make cache checks testable without inspecting the user's model directory.
pub fn inspect_at(
    provider: AsrProvider,
    model: &str,
    root: &Path,
    apple_base: &Path,
    hf_base: &Path,
) -> Result<LocalModelStatus> {
    let mut parts = Vec::new();
    let canonical = match crate::config::canonical_model(provider, model) {
        Ok(model) if provider != AsrProvider::Api => model,
        _ => {
            return Ok(LocalModelStatus {
                provider,
                model: model.trim().into(),
                state: CacheState::Unsupported,
                parts,
                bytes: 0,
                can_prepare: false,
                last_error: None,
                loaded_at: None,
                fingerprint: String::new(),
            });
        }
    };
    let model = canonical.as_str();
    match provider {
        AsrProvider::Cpu | AsrProvider::Gpu => {
            let files = super::llama_paths(root);
            let path = files.model.parent().unwrap().to_path_buf();
            let mut missing = Vec::new();
            for file in [&files.model, &files.mmproj] {
                if !super::file_complete(file) {
                    missing.push(file.file_name().unwrap().to_string_lossy().into_owned());
                }
            }
            parts.push(cache_part("Qwen3-ASR 1.7B Q8_0", path, missing)?);
        }
        AsrProvider::Coreml => {
            for part in apple_parts(model)
                .unwrap_or(&[])
                .iter()
                .chain(std::iter::once(&APPLE_VAD))
            {
                parts.push(apple_part(apple_base, part)?);
            }
        }
        AsrProvider::Npu => {
            let repo = crate::npu::resolve_npu_model(Some(model));
            let root = if Path::new(&repo).is_absolute() {
                PathBuf::from(&repo)
            } else {
                hf_base.join(format!("models--{}", repo.replace('/', "--")))
            };
            let path = if root.join("refs/main").is_file() {
                let revision = fs::read_to_string(root.join("refs/main"))?;
                let revision = revision.trim();
                ensure!(
                    !revision.is_empty()
                        && revision
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-'),
                    crate::tr!("NPU 缓存版本记录无效 / Invalid NPU cache version record")
                );
                root.join("snapshots").join(revision)
            } else {
                root
            };
            let files = files_under(&path)?;
            let xml = files
                .iter()
                .filter(|file| file.extension().is_some_and(|extension| extension == "xml"))
                .collect::<Vec<_>>();
            let mut missing = Vec::new();
            if xml.is_empty() {
                missing.push(
                    crate::tr!("OpenVINO 模型 XML 和权重 / OpenVINO model XML and weights").into(),
                );
            }
            for file in xml {
                if !nonempty(&file.with_extension("bin")) {
                    missing.push(file.with_extension("bin").display().to_string());
                }
            }
            parts.push(cache_part(&repo, path, missing)?);
        }
        AsrProvider::Api => {}
    }
    let bytes = parts.iter().map(|part| part.bytes).sum();
    let missing = parts.iter().any(|part| !part.missing.is_empty());
    let state = if bytes == 0 {
        CacheState::Missing
    } else if missing {
        CacheState::Partial
    } else {
        CacheState::Cached
    };
    let mut hash = Sha256::new();
    hash.update(provider.as_str());
    hash.update(model);
    for part in &parts {
        hash.update(part.path.to_string_lossy().as_bytes());
        for (name, len, modified) in &part.files {
            hash.update(name.as_bytes());
            hash.update(len.to_le_bytes());
            hash.update(modified.to_le_bytes());
        }
    }
    Ok(LocalModelStatus {
        provider,
        model: model.into(),
        state,
        parts,
        bytes,
        can_prepare: true,
        last_error: None,
        loaded_at: None,
        fingerprint: hex(&hash.finalize()),
    })
}
fn cache_part(name: &str, path: PathBuf, missing: Vec<String>) -> Result<CachePart> {
    let mut files = Vec::new();
    for file in files_under(&path)? {
        let metadata = fs::metadata(&file)?;
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let relative = file.strip_prefix(&path).unwrap_or(&file);
        files.push((
            relative.to_string_lossy().into_owned(),
            metadata.len(),
            modified,
        ));
    }
    Ok(CachePart {
        name: name.into(),
        bytes: files.iter().map(|(_, len, _)| len).sum(),
        path,
        missing,
        files,
    })
}
fn apple_part(base: &Path, part: &ApplePart) -> Result<CachePart> {
    let ApplePart {
        repo,
        files,
        bundles,
        weights,
    } = *part;
    let old = base.join(repo.replace('/', "_"));
    // Match speech-swift's directory choice exactly, then independently assess file
    // completeness. Its legacy probe accepts bundle markers, but rejects missing shards.
    let path = if apple_uses_legacy_cache(&old)? {
        old
    } else {
        base.join("models").join(repo)
    };
    let mut missing = files
        .iter()
        .filter(|file| !nonempty(&path.join(file)))
        .map(|file| (*file).into())
        .collect::<Vec<String>>();
    for bundle in bundles {
        let payload = files_under(&path.join(bundle))?;
        if !payload.iter().any(|path| {
            path.extension()
                .is_some_and(|extension| extension == "bin" || extension == "mil")
                && nonempty(path)
        }) {
            missing.push(crate::tr!(
                "{} 的模型内容 / model content of {}",
                bundle,
                bundle
            ));
        }
    }
    if weights {
        let index = path.join("model.safetensors.index.json");
        if index.is_file() {
            match fs::read(index)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|value| {
                    value
                        .get("weight_map")
                        .and_then(|map| map.as_object())
                        .cloned()
                }) {
                Some(map) if !map.is_empty() => {
                    for file in map
                        .values()
                        .filter_map(|value| value.as_str())
                        .collect::<BTreeSet<_>>()
                    {
                        if !safe_tensor(&path.join(file)) {
                            missing.push(file.into());
                        }
                    }
                }
                _ => missing
                    .push(crate::tr!("有效的模型分片索引 / a valid model shard index").into()),
            }
        } else if !files_under(&path)?.iter().any(|file| {
            file.extension()
                .is_some_and(|extension| extension == "safetensors")
                && safe_tensor(file)
        }) {
            missing.push(
                crate::tr!("完整的 safetensors 权重文件 / a complete safetensors weight file")
                    .into(),
            );
        }
    }
    cache_part(repo, path, missing)
}
fn apple_uses_legacy_cache(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let entries = fs::read_dir(path)
        .with_context(|| {
            format!(
                "无法读取模型缓存 / Cannot read the model cache: {}",
                path.display()
            )
        })?
        .collect::<std::io::Result<Vec<_>>>()?;
    if !entries.iter().any(|entry| {
        entry.path().extension().is_some_and(|extension| {
            ["safetensors", "mlmodelc", "mlpackage"]
                .iter()
                .any(|value| extension == *value)
        })
    }) {
        return Ok(false);
    }
    if let Ok(bytes) = fs::read(path.join("model.safetensors.index.json"))
        && let Ok(index) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && let Some(map) = index.get("weight_map").and_then(|value| value.as_object())
        && map.values().all(|value| value.is_string())
        && map
            .values()
            .any(|value| !path.join(value.as_str().unwrap()).exists())
    {
        return Ok(false);
    }
    Ok(true)
}
fn nonempty(path: &Path) -> bool {
    path.is_file() && fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0)
}
fn safe_tensor(path: &Path) -> bool {
    use std::io::Read;
    let checked = || -> Result<bool> {
        let mut file = fs::File::open(path)?;
        let length = file.metadata()?.len();
        let mut header = [0; 8];
        file.read_exact(&mut header)?;
        let size = u64::from_le_bytes(header);
        ensure!(
            size > 0 && size < 16 * 1024 * 1024 && size + 8 < length,
            crate::tr!("权重头不完整 / Incomplete weight header")
        );
        let mut header = vec![0; size as usize];
        file.read_exact(&mut header)?;
        let value: serde_json::Value = serde_json::from_slice(&header)?;
        let tensors = value
            .as_object()
            .context(crate::tr!("权重索引无效 / Invalid weight index"))?;
        let mut count = 0;
        for (key, tensor) in tensors {
            if key == "__metadata__" {
                continue;
            }
            let offsets = tensor
                .get("data_offsets")
                .and_then(|value| value.as_array())
                .context(crate::tr!("权重偏移无效 / Invalid weight offsets"))?;
            ensure!(
                offsets.len() == 2
                    && offsets[1]
                        .as_u64()
                        .is_some_and(|end| end <= length - size - 8),
                crate::tr!("权重文件不完整 / Incomplete weight file")
            );
            count += 1;
        }
        Ok(count > 0)
    };
    checked().unwrap_or(false)
}
/// Every file below `root`, following links, sorted: fingerprints depend on the order.
fn files_under(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let walk = walkdir::WalkDir::new(root).follow_links(true);
    for (count, entry) in walk.into_iter().enumerate() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.loop_ancestor().is_some() => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "无法读取模型缓存 / Cannot read the model cache: {}",
                        root.display()
                    )
                });
            }
        };
        ensure!(
            count < 20_000,
            crate::tr!(
                "模型缓存包含过多文件，检查未完成 / Model cache contains too many files; check incomplete"
            )
        );
        if entry.file_type().is_file() {
            result.push(entry.into_path());
        }
    }
    result.sort();
    Ok(result)
}
fn receipt_path(status: &LocalModelStatus) -> PathBuf {
    let key = format!(
        "{}:{}:{}",
        status.provider.as_str(),
        status.model,
        status
            .parts
            .iter()
            .map(|part| part.path.display().to_string())
            .collect::<Vec<_>>()
            .join("|")
    );
    crate::config::cache_dir()
        .join("model-checks")
        .join(format!("{}.json", hex(&Sha256::digest(key))))
}

pub fn record_result(status: &LocalModelStatus, loaded: bool, error: Option<String>) -> Result<()> {
    let receipt = Receipt {
        fingerprint: status.fingerprint.clone(),
        loaded_at: loaded.then(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        }),
        last_error: error,
        version: env!("CARGO_PKG_VERSION").into(),
    };
    crate::checkpoint::atomic_write(&receipt_path(status), &serde_json::to_vec(&receipt)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn roots() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }
    fn inspect_model(root: &Path, provider: AsrProvider, model: &str) -> LocalModelStatus {
        inspect_at(
            provider,
            model,
            &root.join("gguf"),
            &root.join("apple"),
            &root.join("hub"),
        )
        .unwrap()
    }
    fn tensor(path: &Path, truncate: bool) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let header = br#"{"tensor":{"dtype":"U8","shape":[4],"data_offsets":[0,4]}}"#;
        let mut file = fs::File::create(path).unwrap();
        file.write_all(&(header.len() as u64).to_le_bytes())
            .unwrap();
        file.write_all(header).unwrap();
        file.write_all(if truncate { &[0_u8; 2] } else { &[0_u8; 4] })
            .unwrap();
    }
    fn apple_fixture(
        base: &Path,
        repo: &str,
        weight: bool,
        files: &[&str],
        bundles: &[&str],
    ) -> PathBuf {
        let path = base.join("apple/models").join(repo);
        fs::create_dir_all(&path).unwrap();
        for file in files {
            fs::write(path.join(file), "{}").unwrap();
        }
        for bundle in bundles {
            fs::create_dir_all(path.join(bundle)).unwrap();
            fs::write(path.join(bundle).join("model.mil"), "known fixture payload").unwrap();
        }
        if weight {
            tensor(&path.join("model.safetensors"), false);
        }
        path
    }
    #[test]
    fn inspection_does_not_create_directories_or_claim_an_empty_marker_is_ready() {
        let root = roots();
        let status = inspect_model(root.path(), AsrProvider::Coreml, "qwen3-1.7b");
        assert_eq!(status.state, CacheState::Missing);
        assert_eq!(status.parts.len(), 2);
        assert!(!root.path().join("apple").exists());
        fs::create_dir_all(root.path().join("apple/aufklarer_Qwen3-ASR-1.7B-MLX-8bit")).unwrap();
        let status = inspect_model(root.path(), AsrProvider::Coreml, "qwen3-1.7b");
        assert_eq!(status.state, CacheState::Missing);
        assert!(
            status.parts[0]
                .path
                .ends_with("models/aufklarer/Qwen3-ASR-1.7B-MLX-8bit")
        );
    }
    #[test]
    fn apple_weights_and_vad_are_separate_requirements_and_cache_is_not_a_load_test() {
        let root = roots();
        let main = apple_fixture(
            root.path(),
            "aufklarer/Qwen3-ASR-1.7B-MLX-8bit",
            true,
            &[
                "config.json",
                "vocab.json",
                "merges.txt",
                "tokenizer_config.json",
            ],
            &[],
        );
        let status = inspect_model(root.path(), AsrProvider::Coreml, "qwen3-1.7b");
        assert_eq!(status.state, CacheState::Partial);
        assert!(status.parts[0].missing.is_empty());
        assert!(!status.parts[1].missing.is_empty());
        apple_fixture(
            root.path(),
            "aufklarer/Silero-VAD-v6.2.1-CoreML",
            false,
            &["config.json"],
            &["silero_vad.mlmodelc"],
        );
        let cached = inspect_model(root.path(), AsrProvider::Coreml, "qwen3-1.7b");
        assert_eq!(cached.state, CacheState::Cached);
        assert!(cached.loaded_at.is_none());
        tensor(&main.join("model.safetensors"), true);
        let partial = inspect_model(root.path(), AsrProvider::Coreml, "qwen3-1.7b");
        assert_eq!(partial.state, CacheState::Partial);
        assert_ne!(partial.fingerprint, cached.fingerprint);
    }
    #[test]
    fn missing_safetensor_shard_and_unsupported_model_remain_explicit() {
        let root = roots();
        let main = apple_fixture(
            root.path(),
            "aufklarer/Qwen3-ASR-1.7B-MLX-8bit",
            true,
            &[
                "config.json",
                "vocab.json",
                "merges.txt",
                "tokenizer_config.json",
            ],
            &[],
        );
        fs::write(
            main.join("model.safetensors.index.json"),
            r#"{"weight_map":{"a":"model.safetensors","b":"missing.safetensors"}}"#,
        )
        .unwrap();
        let status = inspect_model(root.path(), AsrProvider::Coreml, "qwen3-1.7b");
        assert!(
            status.parts[0]
                .missing
                .contains(&"missing.safetensors".into())
        );
        let unsupported = inspect_model(root.path(), AsrProvider::Cpu, "qwen3-0.6b");
        assert_eq!(unsupported.state, CacheState::Unsupported);
        assert!(!unsupported.can_prepare);
    }
    #[test]
    fn documented_aliases_resolve_to_the_canonical_model() {
        let root = roots();
        for (provider, alias, canonical) in [
            (AsrProvider::Gpu, "qwen3", "qwen3-1.7b"),
            (AsrProvider::Cpu, " QWEN3-ASR-1.7B ", "qwen3-1.7b"),
            (AsrProvider::Coreml, "qwen3", "qwen3-1.7b"),
            (AsrProvider::Coreml, "whisper-large-v3-turbo", "whisper"),
            (AsrProvider::Coreml, "", "qwen3-1.7b"),
        ] {
            let status = inspect_model(root.path(), provider, alias);
            assert!(status.can_prepare, "{provider} {alias}");
            assert_eq!(status.model, canonical);
        }
        assert!(!inspect_model(root.path(), AsrProvider::Api, "anything").can_prepare);
    }
    #[test]
    fn legacy_directory_choice_matches_loader_before_cache_completeness_is_assessed() {
        let root = roots();
        let old = root.path().join("apple/aufklarer_Qwen3-ASR-1.7B-MLX-8bit");
        tensor(&old.join("model.safetensors"), false);
        fs::write(
            old.join("model.safetensors.index.json"),
            r#"{"weight_map":{"a":"model.safetensors","b":"missing.safetensors"}}"#,
        )
        .unwrap();
        let missing_shard = inspect_model(root.path(), AsrProvider::Coreml, "qwen3-1.7b");
        assert!(
            missing_shard.parts[0]
                .path
                .ends_with("models/aufklarer/Qwen3-ASR-1.7B-MLX-8bit")
        );
        tensor(&old.join("missing.safetensors"), false);
        let complete_index = inspect_model(root.path(), AsrProvider::Coreml, "qwen3-1.7b");
        assert_eq!(complete_index.parts[0].path, old);
        assert!(!complete_index.parts[0].missing.is_empty());

        let old_vad = root.path().join("apple/aufklarer_Silero-VAD-v6.2.1-CoreML");
        fs::create_dir_all(old_vad.join("silero_vad.mlmodelc")).unwrap();
        let bundle_marker = inspect_model(root.path(), AsrProvider::Coreml, "qwen3-1.7b");
        assert_eq!(bundle_marker.parts[1].path, old_vad);
        assert!(!bundle_marker.parts[1].missing.is_empty());
    }
    #[test]
    fn a_large_non_model_response_does_not_pass_gguf_cache_detection() {
        let root = roots();
        let files = super::super::llama_paths(&root.path().join("gguf"));
        fs::create_dir_all(files.model.parent().unwrap()).unwrap();
        for path in [&files.model, &files.mmproj] {
            let file = fs::File::create(path).unwrap();
            file.set_len(1_100_000).unwrap();
        }
        assert_eq!(
            inspect_model(root.path(), AsrProvider::Cpu, "qwen3-1.7b").state,
            CacheState::Partial
        );
        for path in [&files.model, &files.mmproj] {
            let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
            file.write_all(b"GGUF\x03\0\0\0").unwrap();
        }
        assert_eq!(
            inspect_model(root.path(), AsrProvider::Cpu, "qwen3-1.7b").state,
            CacheState::Cached
        );
    }
    #[test]
    fn npu_uses_the_recorded_hub_revision_and_checks_matching_weight_files() {
        let root = roots();
        let repo = root
            .path()
            .join("hub/models--OpenVINO--whisper-large-v3-turbo-int8-ov");
        fs::create_dir_all(repo.join("refs")).unwrap();
        fs::create_dir_all(repo.join("snapshots/abc123")).unwrap();
        fs::write(repo.join("refs/main"), "abc123").unwrap();
        fs::write(
            repo.join("snapshots/abc123/openvino_encoder_model.xml"),
            "<model/>",
        )
        .unwrap();
        let partial = inspect_model(root.path(), AsrProvider::Npu, "whisper");
        assert_eq!(partial.state, CacheState::Partial);
        assert!(partial.parts[0].path.ends_with("snapshots/abc123"));
        fs::write(
            repo.join("snapshots/abc123/openvino_encoder_model.bin"),
            "weights",
        )
        .unwrap();
        assert_eq!(
            inspect_model(root.path(), AsrProvider::Npu, "whisper").state,
            CacheState::Cached
        );
    }
}
