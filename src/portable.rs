//! Exports that retain their contents when moved away from the course library.

use crate::{
    artifact::{self, Document},
    config::OutputFormat,
    execution, render,
};
use anyhow::{Context, Result};
use base64::Engine as _;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

pub fn file_name(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Md => "course.zip",
        OutputFormat::Html => "course.html",
        OutputFormat::Json => "structured.json",
    }
}

/// Exact destination, with no overwrite. UI may explicitly confirm replacing a separate
/// exported file, but must never mutate the immutable note version itself.
pub fn export(version_dir: &Path, format: OutputFormat, destination: &Path) -> Result<PathBuf> {
    export_in_task(
        version_dir,
        format,
        destination,
        &crate::dispatch::TaskControl::default(),
    )
}

/// [`export`] that stops at the task's safe boundaries.
pub(crate) fn export_in_task(
    version_dir: &Path,
    format: OutputFormat,
    destination: &Path,
    control: &crate::dispatch::TaskControl,
) -> Result<PathBuf> {
    let manifest = artifact::read_manifest(&version_dir.join("manifest.json"))?;
    artifact::validate_version_in_task(version_dir, &manifest, control)?;
    let document: Document = serde_json::from_slice(&std::fs::read(artifact::safe_asset_path(
        version_dir,
        &manifest.document,
    )?)?)?;
    write_document(version_dir, &document, format, destination)
}

#[derive(serde::Serialize)]
struct PortableJson<'a> {
    schema: u32,
    title: &'a str,
    author: &'a str,
    duration: Option<f64>,
    source: PortableSource,
    summary: Option<&'a crate::summarize::Summary>,
    sections: Vec<PortableSection<'a>>,
    images: Vec<PortableImage<'a>>,
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum PortableSource {
    Local { file_name: String },
    Online { url: String },
    Unknown,
}

#[derive(serde::Serialize)]
struct PortableSection<'a> {
    t: f64,
    end: f64,
    image_id: Option<&'a str>,
    speech: &'a [crate::timeline::TranscriptEvent],
}

#[derive(serde::Serialize)]
struct PortableImage<'a> {
    id: &'a str,
    mime_type: &'static str,
    sha256: &'a str,
}

struct ImageAsset {
    id: String,
    path: String,
    mime: &'static str,
    bytes: Vec<u8>,
    sha256: String,
}

fn image_mime(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(("image/jpeg", "jpg"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("image/gif", "gif"))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(("image/webp", "webp"))
    } else {
        None
    }
}

fn images(root: &Path, document: &Document) -> Result<BTreeMap<String, ImageAsset>> {
    let mut images = BTreeMap::new();
    for section in document.sections.iter().filter(|s| !s.image.is_empty()) {
        if images.contains_key(&section.image) {
            continue;
        }
        let bytes = std::fs::read(artifact::safe_asset_path(root, &section.image)?)?;
        let sha256 = execution::digest(&bytes);
        let id = format!("image-{}", &sha256[..24]);
        let (mime, ext) = image_mime(&bytes).context(crate::tr!("截图不是可识别的图片，原文件已保留 / Screenshot is not a recognized image; original file kept"))?;
        images.insert(
            section.image.clone(),
            ImageAsset {
                path: format!("images/{id}.{ext}"),
                id,
                mime,
                bytes,
                sha256,
            },
        );
    }
    Ok(images)
}

pub(crate) fn write_document(
    root: &Path,
    document: &Document,
    format: OutputFormat,
    destination: &Path,
) -> Result<PathBuf> {
    anyhow::ensure!(
        !destination.exists(),
        "导出位置已有文件，未覆盖：{} / Export destination already exists",
        destination.display()
    );
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    crate::checkpoint::make_shared(file.as_file())?;
    let images = images(root, document)?;
    let mut portable = document.clone();
    let local_file = if portable.meta.extractor == "local" {
        let name = Path::new(&portable.meta.webpage_url)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned());
        portable.meta.webpage_url.clear();
        name
    } else {
        None
    };
    // HTML 内嵌图片：渲染前直接把路径换成 data URI，避免对整份文档逐图做字符串替换
    let inline = format == OutputFormat::Html;
    for section in &mut portable.sections {
        if let Some(image) = images.get(&section.image) {
            section.image = if inline {
                format!(
                    "data:{};base64,{}",
                    image.mime,
                    base64::engine::general_purpose::STANDARD.encode(&image.bytes)
                )
            } else {
                image.path.clone()
            };
        }
    }
    match format {
        OutputFormat::Md => {
            let mut text = render::render_markdown(&portable.meta, &portable.sections);
            if let Some(summary) = &portable.summary {
                text = crate::summarize::insert_into_md(&text, summary);
            }
            if let Some(name) = &local_file {
                text.push_str(&format!("\n源文件：{}\n", name.replace(['\r', '\n'], " ")));
            }
            let mut zip = zip::ZipWriter::new(file.as_file_mut());
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("course.md", options)?;
            zip.write_all(text.as_bytes())?;
            let mut written = std::collections::HashSet::new();
            for image in images.values() {
                if written.insert(&image.path) {
                    zip.start_file(&image.path, options)?;
                    zip.write_all(&image.bytes)?;
                }
            }
            zip.finish()?;
        }
        OutputFormat::Html => {
            let mut text = render::render_html(&portable.meta, &portable.sections);
            if let Some(summary) = &portable.summary {
                text = crate::summarize::insert_into_html(&text, summary);
            }
            file.write_all(text.as_bytes())?;
        }
        OutputFormat::Json => {
            let source = match (local_file, document.meta.webpage_url.as_str()) {
                (Some(file_name), _) => PortableSource::Local { file_name },
                (None, "") => PortableSource::Unknown,
                (None, url) => PortableSource::Online {
                    url: url.to_owned(),
                },
            };
            let export = PortableJson {
                schema: 1,
                title: &document.meta.title,
                author: &document.meta.uploader,
                duration: (document.meta.duration > 0.).then_some(document.meta.duration),
                source,
                summary: document.summary.as_ref(),
                sections: document
                    .sections
                    .iter()
                    .map(|section| PortableSection {
                        t: section.t,
                        end: section.end,
                        image_id: images.get(&section.image).map(|image| image.id.as_str()),
                        speech: &section.speech,
                    })
                    .collect(),
                images: images
                    .values()
                    .map(|image| PortableImage {
                        id: &image.id,
                        mime_type: image.mime,
                        sha256: &image.sha256,
                    })
                    .collect(),
            };
            file.write_all(&serde_json::to_vec_pretty(&export)?)?;
        }
    }
    file.as_file().sync_all()?;
    file.persist_noclobber(destination).with_context(|| {
        format!(
            "无法保存导出文件；原文件未覆盖 / Could not save export: {}",
            destination.display()
        )
    })?;
    artifact::sync_dir(parent)?;
    Ok(destination.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_exports_survive_without_the_original_library_and_never_overwrite() {
        use crate::{
            fetch::VideoMeta,
            timeline::{Section, TranscriptEvent},
        };
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir_all(source.join("frames")).unwrap();
        image::RgbImage::from_pixel(2, 2, image::Rgb([20, 80, 180]))
            .save(source.join("frames/a.png"))
            .unwrap();
        let document = Document {
            schema: 1,
            meta: VideoMeta {
                title: "title".into(),
                uploader: String::new(),
                duration: 0.,
                webpage_url: "/private/machine/movie.mp4".into(),
                extractor: "local".into(),
                id: "source".into(),
            },
            sections: vec![Section {
                t: 0.,
                end: 1.,
                image: "frames/a.png".into(),
                speech: vec![TranscriptEvent {
                    start: 0.,
                    end: 1.,
                    text: "Actual note".into(),
                    raw: None,
                    translation: None,
                }],
            }],
            summary: None,
        };
        let destination = root.path().join("moved");
        for format in [OutputFormat::Md, OutputFormat::Html, OutputFormat::Json] {
            write_document(
                &source,
                &document,
                format,
                &destination.join(file_name(format)),
            )
            .unwrap();
        }
        std::fs::remove_dir_all(source).unwrap();
        let mut zip =
            zip::ZipArchive::new(std::fs::File::open(destination.join("course.zip")).unwrap())
                .unwrap();
        assert_eq!(zip.len(), 2);
        assert!(zip.by_name("course.md").is_ok());
        let html = std::fs::read_to_string(destination.join("course.html")).unwrap();
        assert!(html.contains("data:image/png;base64,"));
        assert!(!html.contains("src=\"images/"));
        assert!(!html.contains("时长 00:00"));
        let json = std::fs::read_to_string(destination.join("structured.json")).unwrap();
        assert!(json.contains("image_id"));
        assert!(!json.contains("/private/machine"));
        assert!(!json.contains("frames/a.png"));
        let existing = destination.join("keep.html");
        std::fs::write(&existing, "user content").unwrap();
        assert!(write_document(root.path(), &document, OutputFormat::Html, &existing).is_err());
        assert_eq!(std::fs::read_to_string(existing).unwrap(), "user content");
    }
}
