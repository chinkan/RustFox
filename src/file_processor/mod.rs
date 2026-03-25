use anyhow::{Context, Result};
use base64::Engine as _;
use std::path::Path;

use crate::config::Config;
use crate::llm::{ContentPart, ImageUrlContent};
use crate::memory::MemoryStore;
use crate::platform::{Attachment, AttachmentKind};

const LONG_CONTEXT_THRESHOLD: usize = 6000;
const CHUNK_SIZE: usize = 1000;
const CHUNK_OVERLAP: usize = 100;



/// Returned by `process_image` to indicate whether we got a vision part or OCR text.
pub enum ImageResult {
    VisionPart(ContentPart),
    OcrText(String),
}

/// Process all attachments for a message.
/// - Images: base64 vision part (if supports_vision) OR OCR text (if not)
/// - PDFs: text extraction
/// - DOCXs: text extraction
/// - Long text (>6000 chars): chunked into knowledge store, RAG-retrieved
pub async fn process_attachments(
    attachments: &[Attachment],
    user_query: &str,
    config: &Config,
    memory: &MemoryStore,
) -> (String, Vec<ContentPart>) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut image_parts: Vec<ContentPart> = Vec::new();

    for attachment in attachments {
        match attachment.kind {
            AttachmentKind::Image => {
                match process_image(
                    &attachment.path,
                    &attachment.mime_type,
                    config.openrouter.supports_vision,
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
                let fname = attachment
                    .file_name
                    .as_deref()
                    .unwrap_or("document.pdf");
                match extract_pdf_text(&attachment.path) {
                    Ok(text) => {
                        let ctx =
                            handle_context_length(&text, fname, user_query, memory).await;
                        text_parts.push(ctx);
                    }
                    Err(e) => {
                        tracing::warn!("PDF extraction failed: {}", e);
                        text_parts.push(format!("[PDF processing failed: {}]", e));
                    }
                }
            }
            AttachmentKind::Docx => {
                let fname = attachment
                    .file_name
                    .as_deref()
                    .unwrap_or("document.docx");
                match extract_docx_text(&attachment.path) {
                    Ok(text) => {
                        let ctx =
                            handle_context_length(&text, fname, user_query, memory).await;
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
    if supports_vision {
        let bytes = tokio::fs::read(path).await?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let data_url = format!("data:{};base64,{}", mime_type, encoded);
        Ok(ImageResult::VisionPart(ContentPart::ImageUrl {
            image_url: ImageUrlContent { url: data_url },
        }))
    } else {
        let text = ocr_image(path, ocr_model_dir).await?;
        Ok(ImageResult::OcrText(text))
    }
}

/// Perform OCR on an image using the ocrs neural-network engine.
/// Downloads model files on first use to `model_dir`.
async fn ocr_image(path: &Path, model_dir: &Path) -> Result<String> {
    ensure_ocr_models(model_dir).await?;

    let det_path = model_dir.join("text-detection.rten");
    let rec_path = model_dir.join("text-recognition.rten");

    let path_owned = path.to_path_buf();

    tokio::task::spawn_blocking(move || -> Result<String> {
        let detection_model = rten::Model::load_file(&det_path)
            .context("Failed to load OCR detection model")?;
        let recognition_model = rten::Model::load_file(&rec_path)
            .context("Failed to load OCR recognition model")?;

        let engine = ocrs::OcrEngine::new(ocrs::OcrEngineParams {
            detection_model: Some(detection_model),
            recognition_model: Some(recognition_model),
            ..Default::default()
        })?;

        let img = image::open(&path_owned)
            .context("Failed to open image for OCR")?
            .into_rgb8();
        let img_source = ocrs::ImageSource::from_bytes(img.as_raw(), img.dimensions())?;
        let ocr_input = engine.prepare_input(img_source)?;
        let text = engine.get_text(&ocr_input)?;
        Ok(text)
    })
    .await
    .context("OCR task panicked")?
}

/// Download OCR model files to model_dir if they don't exist.
async fn ensure_ocr_models(model_dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(model_dir).await?;

    let det = model_dir.join("text-detection.rten");
    let rec = model_dir.join("text-recognition.rten");

    const DET_URL: &str =
        "https://ocrs-models.s3.us-east-1.amazonaws.com/text-detection.rten";
    const REC_URL: &str =
        "https://ocrs-models.s3.us-east-1.amazonaws.com/text-recognition.rten";

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
    let response = reqwest::get(url).await.context("Failed to fetch OCR model")?;
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

/// Extract text content from a PDF file.
fn extract_pdf_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).context("Failed to read PDF")?;
    // unwrap_or_default: malformed PDFs return empty string rather than propagating
    let text = pdf_extract::extract_text_from_mem(&bytes).unwrap_or_default();
    Ok(text)
}

/// Extract text content from a DOCX file.
fn extract_docx_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).context("Failed to read DOCX")?;
    let docx = docx_rs::read_docx(&bytes)
        .map_err(|e| anyhow::anyhow!("Failed to parse DOCX: {:?}", e))?;

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
}
