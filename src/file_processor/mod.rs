use anyhow::{Context, Result};
use base64::Engine as _;
use std::path::Path;

use crate::config::Config;
use crate::llm::{ContentPart, ImageUrlContent};
use crate::memory::MemoryStore;
use crate::platform::{Attachment, AttachmentKind};

mod ocr_chain;

const LONG_CONTEXT_THRESHOLD: usize = 6000;
const CHUNK_SIZE: usize = 1000;
const CHUNK_OVERLAP: usize = 100;

/// Returned by `process_image` to indicate whether we got a vision part or OCR text.
pub enum ImageResult {
    VisionPart(ContentPart),
    OcrText(String),
}

/// Process all attachments for a message.
/// - Images: vision part when the model supports vision, otherwise the OCR
///   chain (RapidOCR PP-OCRv4 `chinese_cht`, optional Tesseract, then `ocrs`)
/// - PDFs: native text. At or under 6000 chars, inject the text. Longer native
///   PDFs stay on the knowledge text-RAG path (1000-char chunks, 100 overlap,
///   top 5). Vision, when enabled, is only the pages those hits already
///   selected (at most 8), never the whole PDF.
/// - DOCXs: text extraction
/// - Long text (>6000 chars): chunked into knowledge store, RAG-retrieved
pub async fn process_attachments(
    attachments: &[Attachment],
    user_query: &str,
    config: &Config,
    memory: &MemoryStore,
    supports_vision: bool,
) -> (String, Vec<ContentPart>) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut image_parts: Vec<ContentPart> = Vec::new();

    for attachment in attachments {
        match attachment.kind {
            AttachmentKind::Image => {
                match process_image(
                    &attachment.path,
                    &attachment.mime_type,
                    supports_vision,
                    &config.ocr.model_dir,
                )
                .await
                {
                    Ok(ImageResult::VisionPart(part)) => image_parts.push(part),
                    Ok(ImageResult::OcrText(text)) => {
                        let fname = attachment.file_name.as_deref().unwrap_or("image");
                        text_parts.push(format!("[Image: {}]\n{}", fname, text));
                    }
                    Err(e) => {
                        tracing::warn!("Image processing failed: {}", e);
                        text_parts.push(format!("[Image processing failed: {}]", e));
                    }
                }
            }
            AttachmentKind::Pdf => {
                let fname = attachment.file_name.as_deref().unwrap_or("document.pdf");
                match std::fs::read(&attachment.path) {
                    Ok(bytes) => {
                        let routed = route_native_pdf(
                            &bytes,
                            fname,
                            user_query,
                            memory,
                            supports_vision,
                            &SystemPdfRenderer,
                        )
                        .await;
                        text_parts.push(routed.text);
                        image_parts.extend(routed.images);
                    }
                    Err(e) => {
                        tracing::warn!("PDF extraction failed: {}", e);
                        text_parts.push(format!("[PDF processing failed: {}]", e));
                    }
                }
            }
            AttachmentKind::Docx => {
                let fname = attachment.file_name.as_deref().unwrap_or("document.docx");
                match extract_docx_text(&attachment.path) {
                    Ok(text) => {
                        let ctx = handle_context_length(&text, fname, user_query, memory).await;
                        text_parts.push(ctx);
                    }
                    Err(e) => {
                        tracing::warn!("DOCX extraction failed: {}", e);
                        text_parts.push(format!("[DOCX processing failed: {}]", e));
                    }
                }
            }
            AttachmentKind::Other => {
                tracing::debug!("Skipping unsupported attachment type");
            }
        }
    }

    (text_parts.join("\n\n"), image_parts)
}

/// Returns either a vision ContentPart (base64) or extracted OCR text.
async fn process_image(
    path: &Path,
    mime_type: &str,
    supports_vision: bool,
    ocr_model_dir: &Path,
) -> Result<ImageResult> {
    match ocr_chain::image_read(supports_vision) {
        ocr_chain::ImageRead::Vision => {
            let bytes = tokio::fs::read(path).await?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let data_url = format!("data:{};base64,{}", mime_type, encoded);
            Ok(ImageResult::VisionPart(ContentPart::ImageUrl {
                image_url: ImageUrlContent { url: data_url },
            }))
        }
        ocr_chain::ImageRead::OcrChain => {
            let text = ocr_image(path, ocr_model_dir).await?;
            Ok(ImageResult::OcrText(text))
        }
    }
}

/// OCR when vision is off. First non-empty text wins.
/// RapidOCR is skipped (not downloaded) unless the pinned files are already present.
/// `ocrs` still fetches its own rten files, and only if it is reached.
async fn ocr_image(path: &Path, model_dir: &Path) -> Result<String> {
    for stage in ocr_chain::ocr_stage_order() {
        let hit = match stage {
            ocr_chain::OcrStage::RapidOcr => ocr_chain::try_rapidocr(path, model_dir).await,
            ocr_chain::OcrStage::Tesseract => ocr_chain::try_tesseract(path).await,
            ocr_chain::OcrStage::Ocrs => Some(ocr_chain::ocr_with_ocrs(path, model_dir).await?),
        };
        if let Some(text) = hit.as_deref().and_then(ocr_chain::accepted_traditional) {
            return Ok(text);
        }
    }
    Ok(String::new())
}

/// Download OCR model files to model_dir if they don't exist.
async fn ensure_ocr_models(model_dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(model_dir).await?;

    let det = model_dir.join("text-detection.rten");
    let rec = model_dir.join("text-recognition.rten");

    const DET_URL: &str = "https://ocrs-models.s3.us-east-1.amazonaws.com/text-detection.rten";
    const REC_URL: &str = "https://ocrs-models.s3.us-east-1.amazonaws.com/text-recognition.rten";

    if !det.exists() {
        tracing::info!("Downloading OCR detection model to {}", det.display());
        download_model(DET_URL, &det).await?;
    }
    if !rec.exists() {
        tracing::info!("Downloading OCR recognition model to {}", rec.display());
        download_model(REC_URL, &rec).await?;
    }
    Ok(())
}

async fn download_model(url: &str, dest: &Path) -> Result<()> {
    let response = reqwest::get(url)
        .await
        .context("Failed to fetch OCR model")?;
    let bytes = response
        .bytes()
        .await
        .context("Failed to read OCR model bytes")?;
    tokio::fs::write(dest, &bytes)
        .await
        .context("Failed to write OCR model")?;
    tracing::info!("OCR model saved: {} bytes", bytes.len());
    Ok(())
}

/// Hard cap from the accepted brief: never one giant vision payload.
/// Retrieval still asks for 5 chunks; this ceiling applies if those hits
/// span more pages than that.
const MAX_VISION_PAGES: usize = 8;
/// Long-edge cap for a retrieved page raster (brief: 1568–2048px).
const PDF_RENDER_LONG_EDGE_PX: &str = "1568";

struct PdfRoute {
    text: String,
    images: Vec<ContentPart>,
}

trait PdfPageRenderer {
    fn render_page(&self, pdf_bytes: &[u8], page: u32) -> Option<Vec<u8>>;
}

struct SystemPdfRenderer;

impl PdfPageRenderer for SystemPdfRenderer {
    fn render_page(&self, pdf_bytes: &[u8], page: u32) -> Option<Vec<u8>> {
        render_page_pdftoppm(pdf_bytes, page)
    }
}

/// Page-split native text. Falls back to one blob when the file parses but
/// pages do not, so the text-RAG path still runs. No OCR.
fn load_pdf_pages(bytes: &[u8]) -> Result<Vec<String>> {
    match pdf_extract::extract_text_from_mem_by_pages(bytes) {
        Ok(pages) => Ok(pages),
        Err(page_err) => match pdf_extract::extract_text_from_mem(bytes) {
            Ok(text) => Ok(vec![text]),
            Err(_) => Err(anyhow::anyhow!("Failed to extract PDF text: {page_err}")),
        },
    }
}

/// `filename::p{page}::chunk_{i}` → 1-based page. Legacy keys without a page
/// stay text-only.
fn page_from_chunk_key(filename: &str, key: &str) -> Option<u32> {
    let rest = key.strip_prefix(filename)?.strip_prefix("::p")?;
    let (num, tail) = rest.split_once("::chunk_")?;
    if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let page = num.parse().ok()?;
    if page == 0 {
        None
    } else {
        Some(page)
    }
}

/// Pages retrieval already picked, in hit order, deduped, capped.
fn vision_pages_from_hits<'a>(
    filename: &str,
    keys: impl IntoIterator<Item = &'a str>,
    cap: usize,
) -> Vec<u32> {
    let mut pages = Vec::new();
    for key in keys {
        let Some(page) = page_from_chunk_key(filename, key) else {
            continue;
        };
        if pages.contains(&page) {
            continue;
        }
        pages.push(page);
        if pages.len() == cap {
            break;
        }
    }
    pages
}

fn format_retrieved_hit(filename: &str, key: &str, value: &str) -> String {
    match page_from_chunk_key(filename, key) {
        Some(page) => format!("[p.{page}]\n{value}"),
        None => value.to_string(),
    }
}

/// Long native PDFs stay on the shipped 6000-char knowledge path.
/// Chunks do not cross a page boundary so a hit maps to one page.
async fn route_native_pdf<R: PdfPageRenderer>(
    bytes: &[u8],
    filename: &str,
    query: &str,
    memory: &MemoryStore,
    supports_vision: bool,
    renderer: &R,
) -> PdfRoute {
    let pages = match load_pdf_pages(bytes) {
        Ok(pages) => pages,
        Err(e) => {
            tracing::warn!("PDF extraction failed: {e}");
            return PdfRoute {
                text: format!("[PDF processing failed: {e}]"),
                images: Vec::new(),
            };
        }
    };
    let joined = pages.join("\n");
    if joined.chars().count() <= LONG_CONTEXT_THRESHOLD {
        return PdfRoute {
            text: format!("[File: {filename}]\n{joined}"),
            images: Vec::new(),
        };
    }

    let mut chunk_i = 0usize;
    for (idx, page_text) in pages.iter().enumerate() {
        if page_text.chars().all(char::is_whitespace) {
            continue;
        }
        let page_no = (idx + 1) as u32;
        for chunk in chunk_text(page_text, CHUNK_SIZE, CHUNK_OVERLAP) {
            let key = format!("{filename}::p{page_no}::chunk_{chunk_i}");
            chunk_i += 1;
            if let Err(e) = memory
                .remember("document_chunk", &key, &chunk, Some(filename))
                .await
            {
                tracing::warn!("Failed to store document chunk {}: {}", chunk_i - 1, e);
            }
        }
    }
    tracing::info!(
        "Document '{}' is {} chars — storing {} page-scoped chunks in knowledge base",
        filename,
        joined.chars().count(),
        chunk_i
    );

    let hits = match memory.search_knowledge(query, 5).await {
        Ok(results) => results,
        Err(e) => {
            tracing::warn!("PDF knowledge search failed: {e}");
            Vec::new()
        }
    };

    let text = if hits.is_empty() {
        format!(
            "[File: {filename} — document indexed, but no relevant sections found for this query]"
        )
    } else {
        let context = hits
            .iter()
            .map(|e| format_retrieved_hit(filename, &e.key, &e.value))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");
        format!("[File: {filename} — relevant sections]\n{context}")
    };

    let images = if supports_vision {
        let pages_for_vision = vision_pages_from_hits(
            filename,
            hits.iter().map(|e| e.key.as_str()),
            MAX_VISION_PAGES,
        );
        render_vision_pages(bytes, &pages_for_vision, renderer)
    } else {
        Vec::new()
    };

    PdfRoute { text, images }
}

fn render_vision_pages<R: PdfPageRenderer>(
    pdf_bytes: &[u8],
    pages: &[u32],
    renderer: &R,
) -> Vec<ContentPart> {
    let mut images = Vec::new();
    for page in pages {
        let Some(png) = renderer.render_page(pdf_bytes, *page) else {
            continue;
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        images.push(ContentPart::ImageUrl {
            image_url: ImageUrlContent {
                url: format!("data:image/png;base64,{encoded}"),
            },
        });
    }
    images
}

fn render_page_pdftoppm(pdf_bytes: &[u8], page: u32) -> Option<Vec<u8>> {
    let tmp = std::env::temp_dir().join(format!(
        "rustfox-pdf-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    if std::fs::create_dir_all(&tmp).is_err() {
        return None;
    }
    let _guard = TmpDir(tmp.clone());
    let pdf_path = tmp.join("in.pdf");
    if std::fs::write(&pdf_path, pdf_bytes).is_err() {
        return None;
    }
    let prefix = tmp.join("page");
    let output = std::process::Command::new("pdftoppm")
        .arg("-png")
        .arg("-scale-to")
        .arg(PDF_RENDER_LONG_EDGE_PX)
        .arg("-f")
        .arg(page.to_string())
        .arg("-l")
        .arg(page.to_string())
        .arg(&pdf_path)
        .arg(&prefix)
        .output();
    let output = match output {
        Ok(output) => output,
        Err(e) => {
            tracing::warn!("pdftoppm failed for retrieved PDF page {page}: {e}");
            return None;
        }
    };
    if !output.status.success() {
        tracing::warn!(
            "pdftoppm exited {} for retrieved PDF page {page}",
            output.status
        );
        return None;
    }
    let png_path = std::fs::read_dir(&tmp).ok()?.find_map(|entry| {
        let path = entry.ok()?.path();
        (path.extension().and_then(|ext| ext.to_str()) == Some("png")).then_some(path)
    })?;
    std::fs::read(png_path).ok()
}

struct TmpDir(std::path::PathBuf);

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Extract text content from a DOCX file.
fn extract_docx_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).context("Failed to read DOCX")?;
    let docx =
        docx_rs::read_docx(&bytes).map_err(|e| anyhow::anyhow!("Failed to parse DOCX: {:?}", e))?;

    let mut text = String::new();
    for child in docx.document.children {
        if let docx_rs::DocumentChild::Paragraph(para) = child {
            for run_child in para.children {
                if let docx_rs::ParagraphChild::Run(run) = run_child {
                    for rc in run.children {
                        if let docx_rs::RunChild::Text(t) = rc {
                            text.push_str(&t.text);
                        }
                    }
                }
            }
            text.push('\n');
        }
    }
    Ok(text)
}

/// Chunk text with overlap.
fn chunk_text(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let end = (start + chunk_size).min(chars.len());
        chunks.push(chars[start..end].iter().collect());
        if end == chars.len() {
            break;
        }
        start += chunk_size - overlap;
    }
    chunks
}

/// If text is long, store chunks in knowledge store and RAG-retrieve relevant ones.
/// If short, return it directly.
async fn handle_context_length(
    text: &str,
    filename: &str,
    query: &str,
    memory: &MemoryStore,
) -> String {
    let char_count = text.chars().count();
    if char_count <= LONG_CONTEXT_THRESHOLD {
        return format!("[File: {}]\n{}", filename, text);
    }

    let chunks = chunk_text(text, CHUNK_SIZE, CHUNK_OVERLAP);
    tracing::info!(
        "Document '{}' is {} chars — storing {} chunks in knowledge base",
        filename,
        char_count,
        chunks.len()
    );

    for (i, chunk) in chunks.iter().enumerate() {
        let key = format!("{}::chunk_{}", filename, i);
        if let Err(e) = memory
            .remember("document_chunk", &key, chunk, Some(filename))
            .await
        {
            tracing::warn!("Failed to store document chunk {}: {}", i, e);
        }
    }

    match memory.search_knowledge(query, 5).await {
        Ok(results) if !results.is_empty() => {
            let context = results
                .iter()
                .map(|e| e.value.as_str())
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");
            format!("[File: {} — relevant sections]\n{}", filename, context)
        }
        _ => format!(
            "[File: {} — document indexed, but no relevant sections found for this query]",
            filename
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_docx_text_reads_paragraph_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("note.docx");
        let file = std::fs::File::create(&path).expect("create docx");
        docx_rs::Docx::new()
            .add_paragraph(
                docx_rs::Paragraph::new().add_run(docx_rs::Run::new().add_text("hello zip")),
            )
            .pack(file)
            .expect("pack docx");
        let text = extract_docx_text(&path).expect("extract docx");
        assert!(
            text.contains("hello zip"),
            "patched docx-rs/zip 9 reader dropped paragraph text: {text:?}"
        );
    }

    #[test]
    fn test_chunk_text_short_returns_one_chunk() {
        let text = "hello world";
        let chunks = chunk_text(text, 1000, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], text);
    }

    #[test]
    fn test_chunk_text_long_splits_with_overlap() {
        let text = "a".repeat(2500);
        let chunks = chunk_text(&text, 1000, 100);
        // chunk 0: [0, 1000)
        // chunk 1: [900, 1900)
        // chunk 2: [1800, 2500) (last chunk, smaller)
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), 1000);
        assert_eq!(chunks[1].chars().count(), 1000);
    }

    #[test]
    fn test_chunk_text_exact_boundary() {
        let text = "b".repeat(1000);
        let chunks = chunk_text(&text, 1000, 100);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test_chunk_text_just_over_boundary() {
        let text = "b".repeat(1001);
        let chunks = chunk_text(&text, 1000, 100);
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn vision_pages_follow_retrieval_and_cap_at_eight() {
        let filename = "manual.pdf";
        let retrieved = [
            format!("{filename}::p40::chunk_0"),
            format!("{filename}::p41::chunk_1"),
            format!("{filename}::p2::chunk_2"),
            format!("{filename}::p40::chunk_3"),
            "other.pdf::p9::chunk_0".to_string(),
            format!("{filename}::chunk_4"),
        ];
        let keys: Vec<&str> = retrieved.iter().map(String::as_str).collect();
        assert_eq!(
            vision_pages_from_hits(filename, keys, MAX_VISION_PAGES),
            vec![40, 41, 2]
        );

        let all_keys: Vec<String> = (1..=120)
            .map(|page| format!("{filename}::p{page}::chunk_{page}"))
            .collect();
        let all_refs: Vec<&str> = all_keys.iter().map(String::as_str).collect();
        let capped = vision_pages_from_hits(filename, all_refs, MAX_VISION_PAGES);
        assert_eq!(capped, (1..=8).collect::<Vec<_>>());
        assert!(capped.len() < 120);
    }

    struct RecordingRenderer {
        pages: std::sync::Mutex<Vec<u32>>,
        png: Vec<u8>,
    }

    impl PdfPageRenderer for RecordingRenderer {
        fn render_page(&self, pdf_bytes: &[u8], page: u32) -> Option<Vec<u8>> {
            assert!(
                pdf_bytes.starts_with(b"%PDF"),
                "renderer receives the PDF only to rasterize a selected page"
            );
            self.pages.lock().expect("pages").push(page);
            Some(self.png.clone())
        }
    }

    fn native_pdf(page_texts: &[String]) -> Vec<u8> {
        use pdf_extract::{Dictionary, Document, Object, Stream};

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut font = Dictionary::new();
        font.set("Type", Object::Name(b"Font".to_vec()));
        font.set("Subtype", Object::Name(b"Type1".to_vec()));
        font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
        let font_id = doc.add_object(Object::Dictionary(font));

        let mut kids = Vec::new();
        for text in page_texts {
            let escaped = text
                .replace('\\', "\\\\")
                .replace('(', "\\(")
                .replace(')', "\\)");
            let content = format!("BT /F1 12 Tf 72 720 Td ({escaped}) Tj ET");
            let content_id = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
            let mut resources = Dictionary::new();
            let mut fonts = Dictionary::new();
            fonts.set("F1", Object::Reference(font_id));
            resources.set("Font", Object::Dictionary(fonts));
            let mut page = Dictionary::new();
            page.set("Type", Object::Name(b"Page".to_vec()));
            page.set("Parent", Object::Reference(pages_id));
            page.set(
                "MediaBox",
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(612),
                    Object::Integer(792),
                ]),
            );
            page.set("Resources", Object::Dictionary(resources));
            page.set("Contents", Object::Reference(content_id));
            let page_id = doc.add_object(Object::Dictionary(page));
            kids.push(Object::Reference(page_id));
        }

        let mut pages = Dictionary::new();
        pages.set("Type", Object::Name(b"Pages".to_vec()));
        pages.set("Kids", Object::Array(kids.clone()));
        pages.set("Count", Object::Integer(kids.len() as i64));
        doc.set_object(pages_id, Object::Dictionary(pages));

        let mut catalog = Dictionary::new();
        catalog.set("Type", Object::Name(b"Catalog".to_vec()));
        catalog.set("Pages", Object::Reference(pages_id));
        let catalog_id = doc.add_object(Object::Dictionary(catalog));
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut buf = Vec::new();
        doc.save_to(&mut buf).expect("save pdf");
        buf
    }

    fn long_native_pdf() -> (Vec<u8>, usize) {
        let mut pages = Vec::new();
        for n in 1..=12 {
            if n == 7 {
                let body = "zebracitation ".repeat(40);
                pages.push(format!("{body} page seven marker"));
            } else {
                pages.push(format!("secretpage{n:02}token ").repeat(40));
            }
        }
        let bytes = native_pdf(&pages);
        (bytes, pages.len())
    }

    #[tokio::test]
    async fn long_native_pdf_text_rag_and_vision_only_on_retrieved_page() {
        let (bytes, page_count) = long_native_pdf();
        assert!(page_count > MAX_VISION_PAGES);
        let extracted = pdf_extract::extract_text_from_mem_by_pages(&bytes).expect("pages");
        assert_eq!(extracted.len(), page_count);
        assert!(
            extracted.join("\n").chars().count() > LONG_CONTEXT_THRESHOLD,
            "fixture must take the long text-RAG path"
        );

        let renderer = RecordingRenderer {
            pages: std::sync::Mutex::new(Vec::new()),
            png: b"\x89PNG\r\n\x1a\nretrieved-page".to_vec(),
        };
        let memory = MemoryStore::open_in_memory().expect("memory");
        let routed = route_native_pdf(
            &bytes,
            "manual.pdf",
            "zebracitation",
            &memory,
            true,
            &renderer,
        )
        .await;

        assert!(routed
            .text
            .contains("[File: manual.pdf — relevant sections]"));
        assert!(routed.text.contains("[p.7]"));
        assert!(routed.text.contains("zebracitation"));
        assert!(!routed.text.contains("secretpage01token"));
        assert!(!routed.text.contains("secretpage12token"));
        assert_eq!(renderer.pages.lock().expect("pages").as_slice(), &[7]);
        assert_eq!(routed.images.len(), 1);
        match &routed.images[0] {
            ContentPart::ImageUrl { image_url } => {
                assert!(image_url.url.starts_with("data:image/png;base64,"));
                assert!(!image_url.url.contains("application/pdf"));
                assert!(!image_url.url.contains("%PDF"));
            }
            ContentPart::Text { .. } => panic!("vision hit must be an image part"),
        }
    }

    #[tokio::test]
    async fn short_native_pdf_stays_inline_and_sends_no_vision() {
        let bytes = native_pdf(&[
            "hello short page one".to_string(),
            "hello short page two".to_string(),
        ]);
        let renderer = RecordingRenderer {
            pages: std::sync::Mutex::new(Vec::new()),
            png: b"png".to_vec(),
        };
        let memory = MemoryStore::open_in_memory().expect("memory");
        let routed = route_native_pdf(&bytes, "note.pdf", "hello", &memory, true, &renderer).await;
        assert!(routed.text.starts_with("[File: note.pdf]\n"));
        assert!(routed.text.contains("hello short page one"));
        assert!(routed.text.contains("hello short page two"));
        assert!(!routed.text.contains("relevant sections"));
        assert!(routed.images.is_empty());
        assert!(renderer.pages.lock().expect("pages").is_empty());
    }

    #[tokio::test]
    async fn long_native_pdf_without_vision_still_cites_retrieved_page() {
        let (bytes, _) = long_native_pdf();
        let renderer = RecordingRenderer {
            pages: std::sync::Mutex::new(Vec::new()),
            png: b"png".to_vec(),
        };
        let memory = MemoryStore::open_in_memory().expect("memory");
        let routed = route_native_pdf(
            &bytes,
            "manual.pdf",
            "zebracitation",
            &memory,
            false,
            &renderer,
        )
        .await;
        assert!(routed.text.contains("[p.7]"));
        assert!(routed.images.is_empty());
        assert!(renderer.pages.lock().expect("pages").is_empty());
    }

    fn pdftoppm_on_path() -> bool {
        std::process::Command::new("pdftoppm")
            .arg("-v")
            .output()
            .is_ok()
    }

    #[test]
    fn system_renderer_rasters_only_the_requested_page() {
        if !pdftoppm_on_path() {
            eprintln!(
                "skipping system_renderer_rasters_only_the_requested_page: pdftoppm not on PATH"
            );
            return;
        }
        let bytes = native_pdf(&["alpha page".to_string(), "beta page".to_string()]);
        let png = SystemPdfRenderer
            .render_page(&bytes, 2)
            .expect("pdftoppm png");
        assert!(png.starts_with(b"\x89PNG"));
    }
}
