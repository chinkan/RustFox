//! Provenance pointers stored in `knowledge.evidence` (ADR-0061).
//!
//! `unknown` is stored on purpose when the writer has no pointer. It is not a
//! trusted source. Readers show [`UNTRUSTED_EVIDENCE_MARK`] instead.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Shown by `recall` and `search_memory` when evidence is missing or `unknown`.
pub const UNTRUSTED_EVIDENCE_MARK: &str = "⚠️ 無來源";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Run,
    Conversation,
    File,
    Url,
    Dream,
    Unknown,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Conversation => "conversation",
            Self::File => "file",
            Self::Url => "url",
            Self::Dream => "dream",
            Self::Unknown => "unknown",
        }
    }
}

/// One `{kind, ref, captured_at}` pointer. `ref` is omitted for `unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidencePointer {
    pub kind: EvidenceKind,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
}

impl EvidencePointer {
    pub fn unknown() -> Self {
        Self {
            kind: EvidenceKind::Unknown,
            reference: None,
            captured_at: None,
        }
    }
}

/// Column JSON is either one pointer or an array of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StoredEvidence {
    Many(Vec<EvidencePointer>),
    One(EvidencePointer),
}

/// Soft backfill only. A dotted basename with a short alphabetic extension
/// counts as a filename. URLs, prose, and version strings do not — those stay
/// `unknown` so a migration never invents a pointer.
pub fn source_looks_like_filename(source: &str) -> bool {
    let s = source.trim();
    if s.is_empty() || s.len() > 512 {
        return false;
    }
    if s.contains("://") || s.contains(['\n', '\r', '\0']) {
        return false;
    }
    let name = s.rsplit(['/', '\\']).next().unwrap_or(s);
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if stem.trim().is_empty() || ext.is_empty() || ext.len() > 8 {
        return false;
    }
    ext.chars().any(|c| c.is_ascii_alphabetic()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
}

pub fn evidence_for_legacy_source(source: Option<&str>) -> StoredEvidence {
    let trimmed = source.map(str::trim).filter(|s| !s.is_empty());
    match trimmed {
        Some(source) if source_looks_like_filename(source) => {
            StoredEvidence::One(EvidencePointer {
                kind: EvidenceKind::File,
                reference: Some(source.to_string()),
                captured_at: None,
            })
        }
        _ => StoredEvidence::One(EvidencePointer::unknown()),
    }
}

pub fn format_evidence_suffix(raw: Option<&str>) -> String {
    let Some(view) = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|raw| serde_json::from_str::<StoredEvidence>(raw).ok())
        .and_then(|stored| trusted_view(&stored))
    else {
        return UNTRUSTED_EVIDENCE_MARK.to_string();
    };
    match serde_json::to_string(&view) {
        Ok(json) => format!("evidence: {json}"),
        Err(_) => UNTRUSTED_EVIDENCE_MARK.to_string(),
    }
}

/// Parse a tool argument. Accepts one object, an array, or a JSON string of
/// either. `unknown` may omit `ref`. Every other kind needs a non-empty `ref`.
pub fn parse_evidence_value(value: &Value) -> Result<StoredEvidence> {
    let value = if let Some(text) = value.as_str() {
        serde_json::from_str::<Value>(text).context("evidence string is not JSON")?
    } else {
        value.clone()
    };
    if !value.is_object() && !value.is_array() {
        anyhow::bail!(
            "evidence must be an object {{kind, ref, captured_at}} or an array of those; kind is run, conversation, file, url, dream, or unknown"
        );
    }
    let parsed: StoredEvidence = serde_json::from_value(value).context(
        "evidence must be {kind, ref, captured_at} or an array; kind is run, conversation, file, url, dream, or unknown",
    )?;
    normalize_stored(parsed)
}

/// `None` (the caller omitted evidence) becomes `{"kind":"unknown"}`.
pub fn evidence_column_json(evidence: Option<&StoredEvidence>) -> Result<String> {
    let unknown = StoredEvidence::One(EvidencePointer::unknown());
    let stored = match evidence {
        None => &unknown,
        Some(StoredEvidence::Many(items)) if items.is_empty() => &unknown,
        Some(other) => other,
    };
    serde_json::to_string(stored).context("serialize knowledge evidence")
}

pub fn evidence_json_value(raw: Option<&str>) -> Value {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Value::Null;
    };
    serde_json::from_str(raw).unwrap_or(Value::Null)
}

/// File-processor write. `ref` is the filename we actually have, not an
/// invented absolute path. `captured_at` is the index time.
pub fn file_evidence(reference: &str) -> StoredEvidence {
    StoredEvidence::One(EvidencePointer {
        kind: EvidenceKind::File,
        reference: Some(reference.trim().to_string()),
        captured_at: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    })
}

fn trusted_view(stored: &StoredEvidence) -> Option<StoredEvidence> {
    match stored {
        StoredEvidence::One(pointer) if pointer.kind == EvidenceKind::Unknown => None,
        StoredEvidence::One(pointer) => Some(StoredEvidence::One(pointer.clone())),
        StoredEvidence::Many(items) => {
            let trusted: Vec<_> = items
                .iter()
                .filter(|pointer| pointer.kind != EvidenceKind::Unknown)
                .cloned()
                .collect();
            if trusted.is_empty() {
                None
            } else {
                Some(StoredEvidence::Many(trusted))
            }
        }
    }
}

fn normalize_stored(stored: StoredEvidence) -> Result<StoredEvidence> {
    match stored {
        StoredEvidence::One(pointer) => Ok(StoredEvidence::One(normalize_pointer(pointer)?)),
        StoredEvidence::Many(items) if items.is_empty() => {
            anyhow::bail!("evidence array must not be empty; omit evidence to store unknown");
        }
        StoredEvidence::Many(items) => {
            let items = items
                .into_iter()
                .map(normalize_pointer)
                .collect::<Result<Vec<_>>>()?;
            Ok(StoredEvidence::Many(items))
        }
    }
}

/// Add `knowledge.evidence` and backfill rows that still have none.
///
/// The FTS update trigger is dropped for the backfill. Evidence is not an FTS
/// column, and a legacy row that was never indexed makes FTS5's `delete`
/// command fail. Key and value are unchanged, so the index stays valid.
pub fn migrate_knowledge_evidence(conn: &Connection) -> Result<()> {
    conn.execute_batch("ALTER TABLE knowledge ADD COLUMN evidence TEXT;")
        .ok();
    conn.execute_batch("DROP TRIGGER IF EXISTS knowledge_fts_update;")?;
    backfill_missing_evidence(conn)?;
    conn.execute_batch(
        "
        CREATE TRIGGER IF NOT EXISTS knowledge_fts_update AFTER UPDATE ON knowledge BEGIN
            INSERT INTO knowledge_fts(knowledge_fts, rowid, key, value)
                VALUES('delete', OLD.rowid, OLD.key, OLD.value);
            INSERT INTO knowledge_fts(rowid, key, value)
                VALUES (NEW.rowid, NEW.key, NEW.value);
        END;
        ",
    )
    .context("restore knowledge_fts_update trigger")?;
    Ok(())
}

fn backfill_missing_evidence(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT rowid, source FROM knowledge WHERE evidence IS NULL OR trim(evidence) = ''",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("read knowledge rows missing evidence")?;
    for (rowid, source) in rows {
        let json = evidence_column_json(Some(&evidence_for_legacy_source(source.as_deref())))?;
        conn.execute(
            "UPDATE knowledge SET evidence = ?1
             WHERE rowid = ?2 AND (evidence IS NULL OR trim(evidence) = '')",
            params![json, rowid],
        )
        .context("backfill knowledge.evidence")?;
    }
    Ok(())
}

fn normalize_pointer(pointer: EvidencePointer) -> Result<EvidencePointer> {
    let reference = pointer
        .reference
        .map(|reference| reference.trim().to_string())
        .filter(|reference| !reference.is_empty());
    let captured_at = pointer
        .captured_at
        .map(|captured_at| captured_at.trim().to_string())
        .filter(|captured_at| !captured_at.is_empty());
    if pointer.kind != EvidenceKind::Unknown && reference.is_none() {
        anyhow::bail!(
            "evidence kind {} requires a non-empty ref",
            pointer.kind.as_str()
        );
    }
    Ok(EvidencePointer {
        kind: pointer.kind,
        reference,
        captured_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn omitted_or_unknown_or_garbage_is_untrusted_mark() {
        assert_eq!(format_evidence_suffix(None), UNTRUSTED_EVIDENCE_MARK);
        assert_eq!(format_evidence_suffix(Some("")), UNTRUSTED_EVIDENCE_MARK);
        assert_eq!(format_evidence_suffix(Some("   ")), UNTRUSTED_EVIDENCE_MARK);
        assert_eq!(
            format_evidence_suffix(Some(r#"{"kind":"unknown"}"#)),
            UNTRUSTED_EVIDENCE_MARK
        );
        assert_eq!(
            format_evidence_suffix(Some("not-json")),
            UNTRUSTED_EVIDENCE_MARK
        );
        assert_eq!(
            format_evidence_suffix(Some(r#"[{"kind":"unknown"},{"kind":"unknown","ref":"x"}]"#)),
            UNTRUSTED_EVIDENCE_MARK
        );
    }

    #[test]
    fn trusted_pointer_is_shown_and_unknown_siblings_are_dropped_from_the_line() {
        let raw = r#"[{"kind":"file","ref":"notes.pdf"},{"kind":"unknown"},{"kind":"run","ref":"run-9","captured_at":"2026-11-03T00:00:00Z"}]"#;
        let line = format_evidence_suffix(Some(raw));
        assert!(
            !line.contains(UNTRUSTED_EVIDENCE_MARK),
            "trusted pointers must not be labeled untrusted: {line}"
        );
        assert!(line.starts_with("evidence: "), "{line}");
        let json = line.trim_start_matches("evidence: ");
        let value: Value = serde_json::from_str(json).unwrap();
        assert_eq!(
            value,
            json!([
                {"kind":"file","ref":"notes.pdf"},
                {"kind":"run","ref":"run-9","captured_at":"2026-11-03T00:00:00Z"}
            ])
        );
    }

    #[test]
    fn single_run_pointer_stays_an_object() {
        let raw = r#"{"kind":"run","ref":"sched-1","captured_at":"2026-11-01T00:00:00Z"}"#;
        let line = format_evidence_suffix(Some(raw));
        let json = line.trim_start_matches("evidence: ");
        let value: Value = serde_json::from_str(json).expect(&line);
        assert_eq!(
            value,
            json!({"kind":"run","ref":"sched-1","captured_at":"2026-11-01T00:00:00Z"})
        );
    }

    #[test]
    fn legacy_backfill_uses_filename_only_when_the_source_looks_like_one() {
        let file = evidence_for_legacy_source(Some("eStatementFile_b0c56c.pdf"));
        assert_eq!(
            file,
            StoredEvidence::One(EvidencePointer {
                kind: EvidenceKind::File,
                reference: Some("eStatementFile_b0c56c.pdf".to_string()),
                captured_at: None,
            })
        );
        assert_eq!(
            evidence_for_legacy_source(Some("  notes.md  ")),
            StoredEvidence::One(EvidencePointer {
                kind: EvidenceKind::File,
                reference: Some("notes.md".to_string()),
                captured_at: None,
            })
        );
        for source in [
            None,
            Some(""),
            Some("   "),
            Some("test"),
            Some("just words"),
            Some("v1.2.3"),
            Some("https://arxiv.org/abs/2610.40350"),
            Some("https://example.com/file.pdf"),
        ] {
            assert_eq!(
                evidence_for_legacy_source(source),
                StoredEvidence::One(EvidencePointer::unknown()),
                "must not invent provenance for {source:?}"
            );
        }
    }

    #[test]
    fn omitted_column_json_is_unknown_not_an_empty_object() {
        let raw = evidence_column_json(None).unwrap();
        assert_eq!(raw, r#"{"kind":"unknown"}"#);
    }

    #[test]
    fn parse_accepts_object_array_and_json_string() {
        let one =
            parse_evidence_value(&json!({"kind":"url","ref":"https://example.com/a"})).unwrap();
        assert_eq!(
            one,
            StoredEvidence::One(EvidencePointer {
                kind: EvidenceKind::Url,
                reference: Some("https://example.com/a".to_string()),
                captured_at: None,
            })
        );
        let many = parse_evidence_value(&json!([
            {"kind":"dream","ref":"know-1"},
            {"kind":"conversation","ref":"conv-1/msg-2","captured_at":"2026-11-07T00:00:00Z"}
        ]))
        .unwrap();
        match many {
            StoredEvidence::Many(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].kind, EvidenceKind::Dream);
                assert_eq!(items[1].reference.as_deref(), Some("conv-1/msg-2"));
            }
            other => panic!("expected array, got {other:?}"),
        }
        let from_string = parse_evidence_value(&json!(
            r#"{"kind":"file","ref":"a.pdf","captured_at":"2026-11-07T01:02:03Z"}"#
        ))
        .unwrap();
        assert_eq!(
            from_string,
            StoredEvidence::One(EvidencePointer {
                kind: EvidenceKind::File,
                reference: Some("a.pdf".to_string()),
                captured_at: Some("2026-11-07T01:02:03Z".to_string()),
            })
        );
    }

    #[test]
    fn parse_rejects_bad_kind_missing_ref_and_empty_array() {
        assert!(parse_evidence_value(&json!({"kind":"guess","ref":"x"})).is_err());
        assert!(parse_evidence_value(&json!({"kind":"file"})).is_err());
        assert!(parse_evidence_value(&json!({"kind":"run","ref":"  "})).is_err());
        assert!(parse_evidence_value(&json!([])).is_err());
        assert!(parse_evidence_value(&json!("not json")).is_err());
        assert!(parse_evidence_value(&json!(42)).is_err());
    }

    #[test]
    fn file_evidence_records_the_name_we_have_and_a_capture_time() {
        let stored = file_evidence("statement.pdf");
        match stored {
            StoredEvidence::One(pointer) => {
                assert_eq!(pointer.kind, EvidenceKind::File);
                assert_eq!(pointer.reference.as_deref(), Some("statement.pdf"));
                assert!(
                    pointer
                        .captured_at
                        .as_deref()
                        .is_some_and(|t| !t.is_empty()),
                    "indexing time is known"
                );
            }
            other => panic!("expected one pointer, got {other:?}"),
        }
    }

    #[test]
    fn evidence_json_value_parses_object_and_nulls_garbage() {
        assert_eq!(evidence_json_value(None), Value::Null);
        assert_eq!(evidence_json_value(Some("nope")), Value::Null);
        assert_eq!(
            evidence_json_value(Some(r#"{"kind":"dream","ref":"know-9"}"#)),
            json!({"kind":"dream","ref":"know-9"})
        );
    }
}
