use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use image::DynamicImage;
use roxmltree::Document;
use scraper::{Html, Selector};
use zip::ZipArchive;

#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub title: String,
    pub authors: Vec<String>,
    pub series: Option<String>,
    pub series_index: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockKind {
    Heading,
    Paragraph,
    Quote,
    Preformatted,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub text: String,
    pub kind: BlockKind,
}

#[derive(Clone, Debug)]
pub struct Chapter {
    pub title: String,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

pub struct Epub {
    archive: ZipArchive<File>,
    package_dir: PathBuf,
    metadata: Metadata,
    manifest: HashMap<String, ManifestItem>,
    spine: Vec<String>,
    cover_id: Option<String>,
}

impl Epub {
    pub fn open(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let mut archive = ZipArchive::new(file)
            .with_context(|| format!("{} is not a valid EPUB/ZIP archive", path.display()))?;
        let container = read_entry(&mut archive, "META-INF/container.xml")
            .context("EPUB is missing META-INF/container.xml")?;
        let container = Document::parse(&container).context("invalid EPUB container document")?;
        let package_path = container
            .descendants()
            .find(|node| node.has_tag_name("rootfile"))
            .and_then(|node| node.attribute("full-path"))
            .context("EPUB container does not identify a package document")?
            .to_owned();
        let package = read_entry(&mut archive, &package_path)
            .with_context(|| format!("EPUB package is missing: {package_path}"))?;
        let package_xml = Document::parse(&package).context("invalid EPUB package document")?;

        let title = first_text(&package_xml, "title").unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        let authors = package_xml
            .descendants()
            .filter(|node| node.has_tag_name("creator"))
            .filter_map(|node| node.text().map(str::trim))
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        let mut series = None;
        let mut series_index = None;
        let mut cover_id = None;
        for node in package_xml
            .descendants()
            .filter(|node| node.has_tag_name("meta"))
        {
            match node.attribute("name") {
                Some("calibre:series") => series = node.attribute("content").map(ToOwned::to_owned),
                Some("calibre:series_index") => {
                    series_index = node.attribute("content").map(ToOwned::to_owned)
                }
                Some("cover") => cover_id = node.attribute("content").map(ToOwned::to_owned),
                _ => {}
            }
            if node.attribute("property") == Some("belongs-to-collection") && series.is_none() {
                series = node
                    .text()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
            }
            if node.attribute("property") == Some("group-position") && series_index.is_none() {
                series_index = node
                    .text()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
            }
        }

        let mut manifest = HashMap::new();
        for node in package_xml
            .descendants()
            .filter(|node| node.has_tag_name("item"))
        {
            let (Some(id), Some(href)) = (node.attribute("id"), node.attribute("href")) else {
                continue;
            };
            manifest.insert(
                id.to_owned(),
                ManifestItem {
                    href: href.to_owned(),
                    media_type: node.attribute("media-type").unwrap_or_default().to_owned(),
                    properties: node.attribute("properties").unwrap_or_default().to_owned(),
                },
            );
        }
        if cover_id.is_none() {
            cover_id = manifest
                .iter()
                .find(|(_, item)| {
                    item.properties
                        .split_whitespace()
                        .any(|p| p == "cover-image")
                })
                .map(|(id, _)| id.clone());
        }
        let spine = package_xml
            .descendants()
            .filter(|node| node.has_tag_name("itemref"))
            .filter_map(|node| node.attribute("idref").map(ToOwned::to_owned))
            .collect::<Vec<_>>();
        if spine.is_empty() {
            bail!("EPUB contains no readable spine items");
        }

        Ok(Self {
            archive,
            package_dir: Path::new(&package_path)
                .parent()
                .unwrap_or(Path::new(""))
                .to_owned(),
            metadata: Metadata {
                title,
                authors,
                series,
                series_index,
            },
            manifest,
            spine,
            cover_id,
        })
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
    pub fn chapter_count(&self) -> usize {
        self.spine.len()
    }

    pub fn cover(&mut self) -> Result<Option<DynamicImage>> {
        let item = self
            .cover_id
            .as_ref()
            .and_then(|id| self.manifest.get(id))
            .cloned()
            .or_else(|| {
                self.manifest
                    .values()
                    .find(|item| item.media_type.starts_with("image/"))
                    .cloned()
            });
        let Some(item) = item else {
            return Ok(None);
        };
        let name = archive_path(&self.package_dir, &item.href);
        let bytes = read_bytes(&mut self.archive, &name)?;
        Ok(Some(
            image::load_from_memory(&bytes).context("failed to decode EPUB cover")?,
        ))
    }

    pub fn chapter(&mut self, index: usize) -> Result<Chapter> {
        let id = self
            .spine
            .get(index)
            .context("chapter index is out of range")?;
        let item = self
            .manifest
            .get(id)
            .with_context(|| format!("EPUB spine item is missing from the manifest: {id}"))?;
        let name = archive_path(&self.package_dir, &item.href);
        let source = read_entry(&mut self.archive, &name)
            .with_context(|| format!("failed to read chapter {name}"))?;
        Ok(parse_chapter(&source, index + 1))
    }
}

fn parse_chapter(source: &str, number: usize) -> Chapter {
    let document = Html::parse_document(source);
    let selector =
        Selector::parse("h1, h2, h3, h4, h5, h6, p, blockquote, pre, li").expect("static selector");
    let mut blocks = Vec::new();
    for element in document.select(&selector) {
        if element.ancestors().any(|node| {
            node.value()
                .as_element()
                .is_some_and(|e| matches!(e.name(), "p" | "blockquote" | "pre" | "li"))
        }) {
            continue;
        }
        let text = normalize_text(&element.text().collect::<Vec<_>>().join(" "));
        if text.is_empty() {
            continue;
        }
        let kind = match element.value().name() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => BlockKind::Heading,
            "blockquote" => BlockKind::Quote,
            "pre" => BlockKind::Preformatted,
            _ => BlockKind::Paragraph,
        };
        blocks.push(Block { text, kind });
    }
    if blocks.is_empty() {
        let text = normalize_text(&document.root_element().text().collect::<Vec<_>>().join(" "));
        if !text.is_empty() {
            blocks.push(Block {
                text,
                kind: BlockKind::Paragraph,
            });
        }
    }
    let title = blocks
        .iter()
        .find(|block| block.kind == BlockKind::Heading)
        .map(|block| block.text.clone())
        .unwrap_or_else(|| format!("Chapter {number}"));
    Chapter { title, blocks }
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn first_text(document: &Document<'_>, name: &str) -> Option<String> {
    document
        .descendants()
        .find(|node| node.has_tag_name(name))
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}
fn archive_path(base: &Path, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    base.join(html_escape::decode_html_entities(href).as_ref())
        .to_string_lossy()
        .replace('\\', "/")
}
fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let bytes = read_bytes(archive, name)?;
    String::from_utf8(bytes).context("EPUB text entry is not UTF-8")
}
fn read_bytes(archive: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("archive entry not found: {name}"))?;
    let mut bytes = Vec::with_capacity(entry.size().try_into().unwrap_or(0));
    entry.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{BlockKind, parse_chapter};
    #[test]
    fn extracts_semantic_blocks_without_nested_duplicates() {
        let chapter = parse_chapter(
            "<html><body><h1>One</h1><blockquote><p>Hello <em>world</em>.</p></blockquote><p>Next.</p></body></html>",
            1,
        );
        assert_eq!(chapter.title, "One");
        assert_eq!(chapter.blocks.len(), 3);
        assert_eq!(chapter.blocks[1].kind, BlockKind::Quote);
        assert_eq!(chapter.blocks[1].text, "Hello world .");
    }
}
