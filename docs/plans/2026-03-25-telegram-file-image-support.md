# Telegram File & Image Support Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Handle Telegram photos and file attachments (PDF, DOCX, images), routing them through a vision/OCR/document extraction pipeline before injecting context into the LLM.

**Architecture:** Five-layer change — (1) `Cargo.toml` deps, (2) platform data model (Attachment), (3) multi-modal LLM messages, (4) new `file_processor` module (image/PDF/DOCX → text/content), (5) telegram handler + agent integration. OCR uses `ocrs` (pure Rust, neural-network-based). Long documents (>6000 chars) are chunked and stored via the existing `knowledge` store + `sqlite-vec` vector DB, then RAG-retrieved per user query.

**Tech Stack:** Rust 2021, Tokio, teloxide 0.17, ocrs 0.12 (OCR), rten 0.24 (model runtime), image 0.25 (image loading), pdf-extract 0.10 (PDF), docx-rs 0.4 (DOCX), infer 0.19 (MIME detection), base64 0.22 (vision encoding)

---

## Reading List

Read before touching any code:

- `src/platform/mod.rs` — IncomingMessage struct (will add `attachments`)
- `src/llm.rs` lines 1–18 — ChatMessage struct (will change `content` type)
- `src/config.rs` lines 44–55 — OpenRouterConfig (will add `supports_vision`)
- `src/platform/telegram.rs` lines 81–100 — handle_message fn (will add photo/doc handling)
- `src/agent.rs` lines 125–215 — process_message (will add attachment processing)
- `src/memory/knowledge.rs` lines 19–78 — `remember` and `search_knowledge` (reused for long-doc RAG)

---

## Task 1: Add Dependencies

**Files:**
- Modify: `Cargo.toml`

Add under `[dependencies]`:
```toml
# OCR (pure Rust, neural-network based)
ocrs = "0.12"
rten = { version = "0.24", features = ["rten_format"] }

# Image loading/processing
image = { version = "0.25", default-features = false, features = ["jpeg", "png", "gif", "webp"] }

# Document processing
pdf-extract = "0.10"
docx-rs = "0.4"

# MIME type detection
infer = "0.19"

# Base64 for vision API content parts
base64 = "0.22"
```

**Step 1:** Edit `Cargo.toml`

**Step 2:** Run `cargo check` to verify deps resolve

**Step 3:** Commit: `feat: add file processing dependencies`

---

## Task 2: Platform Data Model — Attachment

**Files:**
- Modify: `src/platform/mod.rs`

Add `AttachmentKind` enum, `Attachment` struct, and `attachments` field to `IncomingMessage`:

```rust
/// What kind of attachment was received
#[derive(Debug, Clone, PartialEq)]
pub enum AttachmentKind {
    Image,
    Pdf,
    Docx,
    Other,
}

/// A file attachment received from a platform
#[derive(Debug, Clone)]
pub struct Attachment {
    pub kind: AttachmentKind,
    /// Absolute path to the downloaded temp file
    pub path: std::path::PathBuf,
    pub mime_type: String,
    /// Original filename, if known
    pub file_name: Option<String>,
}

/// A message received from any platform
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IncomingMessage {
    pub platform: String,
    pub user_id: String,
    pub chat_id: String,
    pub user_name: String,
    pub text: String,
    /// Attached files, if any
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}
```

**Step 1:** Edit `src/platform/mod.rs`

**Step 2:** Fix any existing `IncomingMessage { ... }` construction sites that now need `attachments: vec![]` (check `src/agent.rs` and `src/platform/telegram.rs`). Grep: `IncomingMessage {`

**Step 3:** Run `cargo check`

**Step 4:** Commit: `feat: add Attachment type to IncomingMessage`

---

## Task 3: Multi-Modal ChatMessage

**Files:**
- Modify: `src/llm.rs`

Change `ChatMessage.content` from `Option<String>` to `MessageContent` which can be a plain string (for tool result messages) or a vec of content parts (for vision messages). Keep backward-compatible serialization.

```rust
/// A single part in a multi-modal message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlContent },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrlContent {
    /// "data:image/jpeg;base64,..." or a URL
    pub url: String,
}

/// Either a plain text string or a list of content parts (multi-modal)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// Extract all text from the content (for logging, RAG, etc.)
    pub fn as_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|p| if let ContentPart::Text { text } = p { Some(text.as_str()) } else { None })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
    pub fn from_text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }
}

pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    ...
}
```

**Note on backward compat:** All places that currently do `content: Some("...".to_string())` must change to `content: Some(MessageContent::from_text("..."))`. Places that read `.content` as string need `.content.as_ref().map(|c| c.as_text())` or `.content.as_deref()` (removed in favour of as_text).

**Step 1:** Edit `src/llm.rs` — add types above `ChatMessage`, update `ChatMessage.content`

**Step 2:** Update all construction/access sites in `llm.rs` and `agent.rs` (search: `.content.as_deref()`, `content: Some(`)

**Step 3:** Update `src/memory/conversations.rs` if it constructs `ChatMessage` directly (grep: `ChatMessage {`)

**Step 4:** Update `src/platform/telegram.rs` `IncomingMessage` construction (not ChatMessage, just ensure it compiles)

**Step 5:** Run `cargo check` — fix all type errors

**Step 6:** Run `cargo test` 

**Step 7:** Commit: `feat: multi-modal ChatMessage content type`

---

## Task 4: Config — Vision Support + OCR Model Dir

**Files:**
- Modify: `src/config.rs`
- Modify: `config.example.toml`

Add to `OpenRouterConfig`:
```rust
#[serde(default)]
pub supports_vision: bool,
```

Add `OcrConfig`:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct OcrConfig {
    /// Directory to cache OCR model files (downloaded on first use)
    #[serde(default = "default_ocr_model_dir")]
    pub model_dir: PathBuf,
}

fn default_ocr_model_dir() -> PathBuf {
    dirs_next::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ocrs")
}
```

Add to `Config`:
```rust
#[serde(default = "default_ocr_config")]
pub ocr: OcrConfig,
```

Note: `dirs_next` is not a dependency — use `std::env::var("HOME")` fallback instead:
```rust
fn default_ocr_model_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".cache/ocrs")
}
```

**Step 1:** Edit `src/config.rs`
**Step 2:** Edit `config.example.toml` — document new fields
**Step 3:** Run `cargo check`
**Step 4:** Commit: `feat: add vision support and OCR config`

---

## Task 5: File Processor Module

**Files:**
- Create: `src/file_processor/mod.rs`
- Modify: `src/main.rs` (add `mod file_processor;`)

This is the core new module. It exposes:
- `process_attachments(attachments, user_query, config, memory) -> ProcessedAttachments`
- `ProcessedAttachments { text_context: String, image_parts: Vec<ContentPart> }`

### Sub-task 5a: Image processing (vision or OCR)

```rust
/// Returns a ContentPart::ImageUrl if vision-capable model, or extracted text via OCR.
pub async fn process_image_attachment(
    path: &Path,
    mime_type: &str,
    supports_vision: bool,
    ocr_model_dir: &Path,
) -> Result<ImageResult> {
    if supports_vision {
        let bytes = std::fs::read(path)?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let data_url = format!("data:{};base64,{}", mime_type, encoded);
        Ok(ImageResult::VisionPart(ContentPart::ImageUrl {
            image_url: ImageUrlContent { url: data_url }
        }))
    } else {
        let text = ocr_image(path, ocr_model_dir).await?;
        Ok(ImageResult::OcrText(text))
    }
}
```

OCR using `ocrs`:
```rust
async fn ocr_image(path: &Path, model_dir: &Path) -> Result<String> {
    let det_path = model_dir.join("text-detection.rten");
    let rec_path = model_dir.join("text-recognition.rten");
    
    // Download models if not cached
    ensure_ocr_models(model_dir).await?;
    
    let detection_model = rten::Model::load_file(&det_path)?;
    let recognition_model = rten::Model::load_file(&rec_path)?;
    
    let engine = ocrs::OcrEngine::new(ocrs::OcrEngineParams {
        detection_model: Some(detection_model),
        recognition_model: Some(recognition_model),
        ..Default::default()
    })?;
    
    let img = image::open(path)?.into_rgb8();
    let img_source = ocrs::ImageSource::from_bytes(img.as_raw(), img.dimensions())?;
    let ocr_input = engine.prepare_input(img_source)?;
    let text = engine.get_text(&ocr_input)?;
    Ok(text)
}

async fn ensure_ocr_models(model_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(model_dir)?;
    let det = model_dir.join("text-detection.rten");
    let rec = model_dir.join("text-recognition.rten");
    
    const DET_URL: &str = "https://ocrs-models.s3.us-east-1.amazonaws.com/text-detection.rten";
    const REC_URL: &str = "https://ocrs-models.s3.us-east-1.amazonaws.com/text-recognition.rten";
    
    if !det.exists() {
        download_file(DET_URL, &det).await?;
    }
    if !rec.exists() {
        download_file(REC_URL, &rec).await?;
    }
    Ok(())
}
```

### Sub-task 5b: PDF processing

```rust
pub fn extract_pdf_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    let text = pdf_extract::extract_text_from_mem(&bytes)
        .unwrap_or_default();
    Ok(text)
}
```

Note: `pdf-extract` does not expose easy image extraction API. We extract text only from PDFs for now.

### Sub-task 5c: DOCX processing

```rust
pub fn extract_docx_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    let docx = docx_rs::read_docx(&bytes)?;
    let mut text = String::new();
    for child in docx.document.children {
        if let docx_rs::DocumentChild::Paragraph(para) = child {
            for run in para.children {
                if let docx_rs::ParagraphChild::Run(run) = run {
                    for rc in run.children {
                        if let docx_rs::RunChild::Text(t) = rc {
                            text.push_str(&t.text);
                        }
                    }
                    text.push('\n');
                }
            }
        }
    }
    Ok(text)
}
```

### Sub-task 5d: Long-context chunking

```rust
const LONG_CONTEXT_THRESHOLD: usize = 6000;
const CHUNK_SIZE: usize = 1000;
const CHUNK_OVERLAP: usize = 100;

/// If text is long, store as knowledge chunks and RAG-retrieve relevant ones.
/// Returns a context block appropriate for injection.
pub async fn handle_long_context(
    text: &str,
    filename: &str,
    query: &str,
    memory: &MemoryStore,
) -> Result<String> {
    if text.chars().count() <= LONG_CONTEXT_THRESHOLD {
        return Ok(format!("[File: {}]\n{}", filename, text));
    }
    
    // Chunk and store
    let chunks = chunk_text(text, CHUNK_SIZE, CHUNK_OVERLAP);
    for (i, chunk) in chunks.iter().enumerate() {
        let key = format!("{}::chunk_{}", filename, i);
        memory.remember("document_chunk", &key, chunk, Some(filename)).await?;
    }
    
    // RAG-retrieve relevant chunks
    let results = memory.search_knowledge(query, 5).await?;
    let context = results.iter()
        .map(|e| e.value.as_str())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    
    Ok(format!("[File: {} — relevant sections]\n{}", filename, context))
}
```

**Step 1:** Create `src/file_processor/mod.rs` with all the above

**Step 2:** Add `mod file_processor;` to `src/main.rs`

**Step 3:** Run `cargo check` — iterate on type errors

**Step 4:** Run `cargo test`

**Step 5:** Commit: `feat: file processor module (image OCR/vision, PDF, DOCX)`

---

## Task 6: Telegram Handler — Download Photos & Documents

**Files:**
- Modify: `src/platform/telegram.rs`
- Modify: `src/platform/mod.rs` (already done in Task 2)

In `handle_message`, before the text-only early return, add handling for photo and document:

```rust
async fn handle_message(bot: Bot, msg: Message, agent: Arc<Agent>) -> ResponseResult<()> {
    let user = match msg.from.as_ref() { ... };

    // Determine text content (may be empty if message is photo/doc only)
    let text = msg.text()
        .or_else(|| msg.caption())  // use caption for media messages
        .unwrap_or("")
        .to_string();

    // Collect attachments
    let mut attachments = Vec::new();
    let temp_dir = std::env::temp_dir().join(format!("rustfox_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).ok();

    // Handle photo
    if let Some(photos) = msg.photo() {
        if let Some(largest) = photos.last() {
            match download_telegram_file(&bot, &largest.file.id, &temp_dir, None).await {
                Ok((path, mime)) => attachments.push(Attachment {
                    kind: AttachmentKind::Image,
                    path,
                    mime_type: mime,
                    file_name: None,
                }),
                Err(e) => warn!("Failed to download photo: {}", e),
            }
        }
    }

    // Handle document
    if let Some(doc) = msg.document() {
        let file_name = doc.file_name.clone();
        let kind = classify_document_kind(&doc.mime_type, &file_name);
        match download_telegram_file(&bot, &doc.file.id, &temp_dir, file_name.as_deref()).await {
            Ok((path, mime)) => attachments.push(Attachment {
                kind,
                path,
                mime_type: mime,
                file_name,
            }),
            Err(e) => warn!("Failed to download document: {}", e),
        }
    }

    // Skip if nothing to process
    if text.is_empty() && attachments.is_empty() {
        return Ok(());
    }

    // ... existing command handling and streaming setup ...

    let incoming = IncomingMessage {
        platform: "telegram".to_string(),
        user_id: user_id.to_string(),
        chat_id: msg.chat.id.0.to_string(),
        user_name,
        text,
        attachments,
    };

    // Cleanup temp dir after processing
    let process_result = agent.process_message(&incoming, tool_event_tx, Some(stream_token_tx)).await;
    std::fs::remove_dir_all(&temp_dir).ok();
    
    ...
}

/// Download a Telegram file to temp_dir. Returns (path, mime_type).
async fn download_telegram_file(
    bot: &Bot,
    file_id: &str,
    temp_dir: &Path,
    filename: Option<&str>,
) -> Result<(PathBuf, String)> {
    use teloxide::net::Download;
    
    let file = bot.get_file(file_id).await.context("get_file failed")?;
    let ext = Path::new(&file.path).extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let dest_name = filename.map(String::from)
        .unwrap_or_else(|| format!("{}.{}", uuid::Uuid::new_v4(), ext));
    let dest = temp_dir.join(&dest_name);
    
    let mut bytes: Vec<u8> = Vec::new();
    bot.download_file(&file.path, &mut bytes).await.context("download_file failed")?;
    std::fs::write(&dest, &bytes)?;
    
    // Detect MIME
    let mime = infer::get(&bytes)
        .map(|t| t.mime_type().to_string())
        .unwrap_or_else(|| mime_from_ext(ext));
    
    Ok((dest, mime))
}

fn classify_document_kind(mime: &Option<String>, filename: &Option<String>) -> AttachmentKind {
    let mime_str = mime.as_deref().unwrap_or("");
    let name_str = filename.as_deref().unwrap_or("");
    if mime_str.starts_with("image/") { return AttachmentKind::Image; }
    if mime_str == "application/pdf" || name_str.ends_with(".pdf") { return AttachmentKind::Pdf; }
    if mime_str.contains("wordprocessingml") || name_str.ends_with(".docx") { return AttachmentKind::Docx; }
    AttachmentKind::Other
}

fn mime_from_ext(ext: &str) -> String {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "application/octet-stream",
    }.to_string()
}
```

**Step 1:** Edit `src/platform/telegram.rs`

**Step 2:** Run `cargo check`

**Step 3:** Run `cargo test`

**Step 4:** Commit: `feat: telegram handler downloads photos and documents`

---

## Task 7: Agent — Process Attachments

**Files:**
- Modify: `src/agent.rs`

In `process_message()`, after building the `user_msg`, check for attachments and process them:

```rust
// Process attachments into text context and/or vision content parts
let (attachment_text, image_parts) = if !incoming.attachments.is_empty() {
    crate::file_processor::process_attachments(
        &incoming.attachments,
        &incoming.text,
        &self.config,
        &self.memory,
    ).await
} else {
    (String::new(), vec![])
};

// Build user message: text + attachment context + optional image parts
let user_msg_content = if image_parts.is_empty() {
    // Text-only: combine user text + any extracted document text
    let mut combined = incoming.text.clone();
    if !attachment_text.is_empty() {
        combined.push_str("\n\n");
        combined.push_str(&attachment_text);
    }
    MessageContent::from_text(combined)
} else {
    // Multi-modal: text part + image parts
    let mut parts = Vec::new();
    let mut text_content = incoming.text.clone();
    if !attachment_text.is_empty() {
        text_content.push_str("\n\n");
        text_content.push_str(&attachment_text);
    }
    if !text_content.is_empty() {
        parts.push(ContentPart::Text { text: text_content });
    }
    parts.extend(image_parts);
    MessageContent::Parts(parts)
};

let user_msg = ChatMessage {
    role: "user".to_string(),
    content: Some(user_msg_content),
    tool_calls: None,
    tool_call_id: None,
};
```

Also update the RAG retrieval to use `incoming.text` as the query (unchanged), and the message saved to DB: save with text-only content (strip image parts for DB storage to avoid bloat):

```rust
// Save text-only version to DB (don't store base64 image data in message history)
let db_msg = ChatMessage {
    role: "user".to_string(),
    content: Some(MessageContent::from_text({
        let mut t = incoming.text.clone();
        if !attachment_text.is_empty() {
            t.push_str("\n\n[Attachment processed]");
        }
        t
    })),
    tool_calls: None,
    tool_call_id: None,
};
self.memory.save_message(&conversation_id, &db_msg).await?;
messages.push(user_msg); // push the full message (with images) to in-memory context only
```

**Step 1:** Edit `src/agent.rs`

**Step 2:** Run `cargo check`

**Step 3:** Run `cargo test`

**Step 4:** Commit: `feat: agent processes file attachments`

---

## Task 8: Final Wiring and Tests

**Step 1:** Run `cargo clippy -- -D warnings` and fix all warnings

**Step 2:** Run `cargo test`

**Step 3:** Add unit tests for:
- `classify_document_kind()` in `telegram.rs`
- `chunk_text()` in `file_processor/mod.rs`
- `MessageContent` serialization (text stays as string, parts serialize correctly)

**Step 4:** Commit: `test: add unit tests for file attachment pipeline`

---

## Notes on OCR Model Download

`ocrs` requires two `.rten` model files. On first OCR use:
1. If `~/.cache/ocrs/text-detection.rten` doesn't exist → download from S3
2. Same for `text-recognition.rten`

This is done by `ensure_ocr_models()` in the file_processor. The download uses `reqwest` (already a dependency). Models are ~100MB total; download is one-time.

If the bot is deployed without internet access, operators should pre-download models and point `[ocr] model_dir` to their location in config.toml.

## config.example.toml additions

```toml
[openrouter]
# ... existing fields ...
# Set to true if your model supports vision (image inputs)
# supports_vision = false

[ocr]
# Directory where OCR model files are cached (downloaded on first use)
# model_dir = "~/.cache/ocrs"
```
