pub mod embed;
pub mod loader;
/// Names that are never real skills/agents: update-engine backups
/// (`foo.bak`), timestamped skill-file backups (`SKILL.md.bak-YYYYMMDD-HHMMSS`),
/// portal quarantines (`foo.deleted-<ts>`), and dot-prefixed entries
/// (`.trash/`, `.DS_Store`, `.git`). Shared by the filesystem loader and the
/// portal listings so both agree on what exists (ADR 0011a R3).
pub fn is_hidden_entry(name: &str) -> bool {
    name.ends_with(".bak")
        || name.contains(".bak-")
        || name.contains(".deleted-")
        || name.starts_with('.')
}

pub mod seed;
pub mod update;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Build the shared preamble for a "listed below" section that warns the LLM
/// not to enumerate files itself. `noun_singular` is used in both the intro
/// ("All available {noun}s are listed below.") and the warning
/// ("DO NOT try to list {noun} directories or files"). `followup` is appended
/// after the preamble and before the listed items.
///
/// This is shared between `build_context` (skills) and the agents section in
/// `agent.rs::build_system_prompt` to keep the wording consistent.
pub fn format_listed_section(noun_singular: &str, followup: &str) -> String {
    format!(
        "All available {0}s are listed below. DO NOT try to list {0} directories or files — everything you need is documented here.\n\n{1}\n\n",
        noun_singular, followup
    )
}

/// Max chars of a skill/agent description shown in a system-prompt catalog line
/// outside `# Available Skills` (agents and subagents). Instruction skills use
/// [`CATALOG_TRIGGER_MAX_BYTES`] instead (ADR-0060).
const CATALOG_DESCRIPTION_MAX_CHARS: usize = 200;

/// Max UTF-8 bytes of trigger prose on one `# Available Skills` line.
///
/// 24 is the largest cap that keeps an 83-skill section — heading, preamble,
/// realistic names, prose at the cap plus `…`, and one synthetic
/// `Load with: read_skill_file(...)` per skill — within 12 KiB. The old
/// ~6.8KB / 80-byte figure left out that mandatory tool clause.
pub const CATALOG_TRIGGER_MAX_BYTES: usize = 24;

/// `# Available Skills` byte budget, including the heading (ADR-0060).
pub const AVAILABLE_SKILLS_SECTION_BUDGET_BYTES: usize = 12 * 1024;

/// Cap on an author-written `Load with:` clause so one verbose skill cannot
/// blow the section budget. The synthetic clause is shorter than this.
const CATALOG_LOAD_WITH_MAX_BYTES: usize = 180;

const LOAD_WITH_PREFIX: &str = "Load with:";

/// Render a description for a catalog line: first non-empty line only, capped
/// at ~200 chars on a char boundary (`…` appended when cut). The stored
/// `Skill.description` is never modified; only the catalog rendering is.
pub fn catalog_description(s: &str) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    match line.char_indices().nth(CATALOG_DESCRIPTION_MAX_CHARS) {
        Some((idx, _)) => format!("{}…", line[..idx].trim_end()),
        None => line.to_string(),
    }
}

/// First non-empty trimmed line. Catalog lines are single-line.
fn first_nonempty_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// `trigger:` when it has text; otherwise the description. Blank trigger
/// falls back so a whitespace-only field does not hide the description.
fn catalog_source<'a>(trigger: Option<&'a str>, description: &'a str) -> &'a str {
    trigger
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(description)
}

/// Prose before an author `Load with:` on the same line. The clause is
/// attached separately so it is not duplicated and is not eaten by the cap.
fn strip_load_with_prose(line: &str) -> &str {
    match line.find(LOAD_WITH_PREFIX) {
        Some(i) => line[..i].trim(),
        None => line.trim(),
    }
}

/// Truncate on a UTF-8 char boundary. `…` is appended only when cut.
fn truncate_utf8_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", s[..end].trim_end())
}

/// The first `Load with:` clause in `text`, stopping before a second one on
/// the same line. `None` when the marker is absent.
fn author_load_with_clause(text: &str) -> Option<&str> {
    let idx = text.find(LOAD_WITH_PREFIX)?;
    let rest = &text[idx..];
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let mut line = rest[..line_end].trim();
    if line.len() > LOAD_WITH_PREFIX.len() {
        if let Some(rel) = line[LOAD_WITH_PREFIX.len()..].find(LOAD_WITH_PREFIX) {
            line = line[..LOAD_WITH_PREFIX.len() + rel]
                .trim()
                .trim_end_matches('.')
                .trim();
        }
    }
    if line.is_empty() {
        None
    } else {
        Some(line)
    }
}

fn synthetic_load_with(name: &str) -> String {
    format!("Load with: read_skill_file(skill_name=\"{name}\", relative_path=\"SKILL.md\")")
}

/// Exactly one load clause. An author clause that already names
/// `read_skill_file` is kept (so a non-SKILL.md path is not rewritten).
/// Anything else becomes the synthetic clause, which always names the tool.
fn load_with_clause(source: &str, name: &str) -> String {
    if let Some(existing) = author_load_with_clause(source) {
        if existing.contains("read_skill_file") {
            return truncate_utf8_bytes(existing, CATALOG_LOAD_WITH_MAX_BYTES);
        }
    }
    synthetic_load_with(name)
}

/// One `# Available Skills` entry: name, short trigger, one `Load with:`.
fn instruction_catalog_line(skill: &Skill) -> String {
    let source = catalog_source(skill.trigger.as_deref(), &skill.description);
    let prose = truncate_utf8_bytes(
        strip_load_with_prose(first_nonempty_line(source)),
        CATALOG_TRIGGER_MAX_BYTES,
    );
    let clause = load_with_clause(source, &skill.name);
    if prose.is_empty() {
        format!("- **{}**: {clause}", skill.name)
    } else {
        format!("- **{}**: {prose} {clause}", skill.name)
    }
}

/// A loaded skill from a markdown file
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Skill {
    /// Skill name (derived from filename or frontmatter)
    pub name: String,
    /// Short description. Stored in full for the portal and other readers.
    /// The Available Skills catalog does not paste this verbatim (ADR-0060).
    pub description: String,
    /// Optional one-line catalog trigger from frontmatter `trigger:`.
    /// When set, the Available Skills line uses this instead of `description`.
    pub trigger: Option<String>,
    /// Full markdown content (the instructions)
    pub content: String,
    /// Category/tags for organization
    pub tags: Vec<String>,
    /// If set, this skill runs as a subagent using this model
    pub model: Option<String>,
    /// Tool whitelist for the subagent (empty = read_skill_file only)
    pub tools: Vec<String>,
    /// Max loop iterations for the subagent (None = use global config default)
    pub max_iterations: Option<u32>,
    /// If true, use the AGENT.md/SKILL.md body as the system message directly
    /// instead of the "read your instructions" bootstrap step.
    pub skip_bootstrap: bool,
    /// Optional supervisor workflow hint (e.g. "coding", "research", "writing")
    pub supervisor_workflow: Option<String>,
    /// Optional list of capabilities the supervisor should require for this skill's workflow
    pub supervisor_required_caps: Vec<String>,
}

/// Registry of all loaded skills.
///
/// Skills are loaded from the instance directory (under home or configured path).
/// A `skill_base_dirs` map tracks the base directory for each skill so tools like
/// `read_skill_file` can resolve the correct filesystem path.
#[derive(Debug, Clone)]
pub struct SkillRegistry {
    pub skills: HashMap<String, Skill>,
    /// Maps skill name → absolute base directory for read_skill_file path resolution.
    pub skill_base_dirs: HashMap<String, PathBuf>,
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
            skill_base_dirs: HashMap::new(),
        }
    }

    /// Register a skill with its base directory.
    pub fn register(&mut self, skill: Skill, base_dir: PathBuf) {
        let name = skill.name.clone();
        self.skills.insert(name.clone(), skill);
        self.skill_base_dirs.entry(name).or_insert(base_dir);
    }

    /// Get a skill by name.
    #[allow(dead_code)]
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Returns the base directory for a skill, used by read_skill_file for path resolution.
    pub fn base_dir(&self, name: &str) -> Option<&Path> {
        self.skill_base_dirs.get(name).map(|p| p.as_path())
    }

    /// List all skills.
    pub fn list(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    /// Build context string for the system prompt (skills directory).
    /// Instruction skills (no model/tools) are loaded via `read_skill_file` when relevant.
    pub fn build_context(&self) -> String {
        let unique_skills = self.list();
        if unique_skills.is_empty() {
            return String::new();
        }

        let mut instruction_lines = Vec::new();

        for skill in &unique_skills {
            if skill.model.is_none() && skill.tools.is_empty() {
                instruction_lines.push(instruction_catalog_line(skill));
            }
        }

        if instruction_lines.is_empty() {
            return String::new();
        }

        let mut context = format_listed_section(
            "skill",
            "When an instruction skill is relevant, load its full instructions with read_skill_file(skill_name=\"<name>\", relative_path=\"SKILL.md\"), then follow them.\n\n\
             You have the following skills available:",
        );
        context.push_str(&instruction_lines.join("\n"));
        context.push('\n');

        context
    }

    /// `# Available Skills` block inserted into the system prompt.
    /// Empty when there are no instruction skills. The heading is part of the
    /// block so the 12 KiB budget (ADR-0060) covers the whole section.
    pub fn available_skills_section(&self) -> String {
        let body = self.build_context();
        if body.is_empty() {
            String::new()
        } else {
            format!("# Available Skills\n\n{body}")
        }
    }

    /// Build formatted lines for subagent-style skills (those with model or tools).
    /// Returns formatted lines only (no preamble) — caller prepends the unified section header.
    pub fn build_subagent_lines(&self) -> String {
        let unique_skills = self.list();
        let mut lines = Vec::new();

        for skill in &unique_skills {
            if skill.model.is_some() || !skill.tools.is_empty() {
                lines.push(format!(
                    "- **{}**: {}\n  Invoke via: `invoke_agent(agent=\"{}\", prompt=\"<task>\")`",
                    skill.name,
                    catalog_description(&skill.description),
                    skill.name
                ));
            }
        }

        lines.join("\n")
    }

    /// Build formatted lines for the agents directory (agents with their own model/tools).
    /// Returns formatted lines only (no preamble) — caller prepends the unified section header.
    pub fn build_agents_context(&self) -> String {
        let unique_agents = self.list();
        let mut lines = Vec::new();
        for agent in &unique_agents {
            lines.push(format!(
                "- **{}**: {}\n  Invoke via: `invoke_agent(agent=\"{}\", prompt=\"<task>\")`",
                agent.name,
                catalog_description(&agent.description),
                agent.name
            ));
        }

        lines.join("\n")
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn make_skill(name: &str, description: &str, content: &str, model: Option<&str>) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            trigger: None,
            content: content.to_string(),
            tags: vec![],
            model: model.map(str::to_string),
            tools: vec![],
            max_iterations: None,
            skip_bootstrap: false,
            supervisor_workflow: None,
            supervisor_required_caps: vec![],
        }
    }

    #[test]
    fn test_register_and_get() {
        let mut registry = SkillRegistry::new();
        registry.register(
            make_skill("my-skill", "Does stuff", "content", None),
            PathBuf::from("/tmp"),
        );
        let skill = registry.get("my-skill").unwrap();
        assert_eq!(skill.description, "Does stuff");
        assert_eq!(registry.base_dir("my-skill").unwrap(), Path::new("/tmp"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_register_updates_skills_and_base_dir() {
        let mut registry = SkillRegistry::new();
        registry.register(
            make_skill("alpha", "Alpha skill", "", None),
            PathBuf::from("/first"),
        );
        // Re-registering with a different path keeps the first (or_insert behaviour)
        registry.register(
            make_skill("alpha", "Alpha skill", "updated", None),
            PathBuf::from("/second"),
        );
        let skill = registry.get("alpha").unwrap();
        assert_eq!(skill.content, "updated");
        assert_eq!(registry.base_dir("alpha").unwrap(), Path::new("/first"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_build_context_instruction_skill_metadata_only() {
        let mut registry = SkillRegistry::new();
        registry.register(
            make_skill(
                "my-skill",
                "Does things",
                "# Instructions\nDo this and that.",
                None,
            ),
            PathBuf::from("/tmp/test"),
        );
        let ctx = registry.build_context();
        assert!(ctx.contains("my-skill"));
        assert!(ctx.contains("Does things"));
        assert!(ctx.contains("read_skill_file"));
        assert!(ctx.contains("SKILL.md"));
        assert!(!ctx.contains("# Instructions"));
        assert!(!ctx.contains("Do this and that."));
    }

    #[test]
    fn test_build_context_excludes_subagent_skills() {
        let mut registry = SkillRegistry::new();
        registry.register(
            make_skill(
                "thread-writer",
                "Use when writing Thread posts.",
                "# Super Secret Instructions\nLong style guide...",
                Some("anthropic/claude-sonnet-4-6"),
            ),
            PathBuf::from("/tmp/test"),
        );
        let ctx = registry.build_context();
        assert_eq!(ctx, "");
    }

    #[test]
    fn test_build_subagent_context_returns_subagent_skills() {
        let mut registry = SkillRegistry::new();
        registry.register(
            make_skill(
                "thread-writer",
                "Use when writing Thread posts.",
                "# Super Secret Instructions\nLong style guide...",
                Some("anthropic/claude-sonnet-4-6"),
            ),
            PathBuf::from("/tmp/test"),
        );
        let ctx = registry.build_subagent_lines();
        assert!(ctx.contains("thread-writer"));
        assert!(ctx.contains("Use when writing Thread posts."));
        assert!(ctx.contains("invoke_agent"));
        assert!(!ctx.contains("Super Secret Instructions"));
        assert!(!ctx.contains("Long style guide"));
    }

    #[test]
    fn test_build_context_empty_registry() {
        let registry = SkillRegistry::new();
        assert_eq!(registry.build_context(), String::new());
        assert_eq!(registry.build_subagent_lines(), String::new());
        assert_eq!(registry.build_agents_context(), String::new());
    }

    #[test]
    fn test_build_context_instruction_only() {
        let mut registry = SkillRegistry::new();
        registry.register(
            make_skill(
                "instruction-skill",
                "An instruction skill",
                "Follow these instructions.",
                None,
            ),
            PathBuf::from("/tmp/test"),
        );
        registry.register(
            make_skill(
                "subagent-skill",
                "A subagent skill",
                "Secret subagent body.",
                Some("some/model"),
            ),
            PathBuf::from("/tmp/test"),
        );
        let ctx = registry.build_context();
        assert!(ctx.contains("instruction-skill"));
        assert!(ctx.contains("An instruction skill"));
        assert!(ctx.contains("read_skill_file"));
        assert!(!ctx.contains("subagent-skill"));
        assert!(!ctx.contains("invoke_agent"));
        assert!(!ctx.contains("Follow these instructions."));
        assert!(!ctx.contains("Secret subagent body."));

        let subagent_ctx = registry.build_subagent_lines();
        assert!(subagent_ctx.contains("subagent-skill"));
        assert!(subagent_ctx.contains("invoke_agent"));
        assert!(!subagent_ctx.contains("instruction-skill"));
    }

    #[test]
    fn test_catalog_description_caps_long_multiline() {
        let long = format!("{}\nSecond line must not appear.", "a".repeat(500));
        let out = catalog_description(&long);
        assert!(!out.contains("Second line"));
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= CATALOG_DESCRIPTION_MAX_CHARS + 1);
        // Multi-byte chars: truncation stays on a char boundary.
        let cjk = "技".repeat(300);
        let out = catalog_description(&cjk);
        assert_eq!(out.chars().count(), CATALOG_DESCRIPTION_MAX_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn test_catalog_description_short_unchanged_and_never_empty() {
        assert_eq!(catalog_description("Does things"), "Does things");
        assert_eq!(catalog_description(&"b".repeat(200)), "b".repeat(200));
        assert_eq!(catalog_description("\n  \nReal desc\nmore"), "Real desc");
        assert_eq!(catalog_description(""), "");
    }

    #[test]
    fn test_catalog_lines_use_capped_description_and_omit_body() {
        let long = format!("First line {}\nSECOND_LINE_MARKER", "x".repeat(400));
        let body = "DISTINCTIVE_BODY_STRING_42";
        let mut registry = SkillRegistry::new();
        registry.register(
            make_skill("inst", &long, body, None),
            PathBuf::from("/tmp/test"),
        );
        registry.register(
            make_skill("sub", &long, body, Some("some/model")),
            PathBuf::from("/tmp/test"),
        );
        for ctx in [
            registry.build_context(),
            registry.build_subagent_lines(),
            registry.build_agents_context(),
        ] {
            assert!(ctx.contains("First line"));
            assert!(ctx.contains('…'));
            assert!(!ctx.contains("SECOND_LINE_MARKER"));
            assert!(!ctx.contains(body));
            assert!(!ctx.contains(&"x".repeat(400)));
        }
    }

    fn catalog_prose<'a>(ctx: &'a str, name: &str) -> &'a str {
        let marker = format!("**{name}**: ");
        let start = ctx
            .find(&marker)
            .unwrap_or_else(|| panic!("catalog line for {name} missing in:\n{ctx}"));
        let after = &ctx[start + marker.len()..];
        after
            .split(" Load with:")
            .next()
            .unwrap_or_else(|| panic!("Load with clause missing for {name}"))
    }

    #[test]
    fn available_skills_uses_trigger_not_the_long_description() {
        let mut registry = SkillRegistry::new();
        let description = format!(
            "This long description stays on the skill for the portal. {}",
            "PORTAL_ONLY_TAIL ".repeat(30)
        );
        let mut skill = make_skill("rain-alerts", &description, "BODY_STAYS_ON_DISK", None);
        skill.trigger = Some("Rain alerts".into());
        registry.register(skill, PathBuf::from("/tmp/test"));
        let stored = registry.get("rain-alerts").unwrap();
        assert!(stored.description.contains("PORTAL_ONLY_TAIL"));
        assert_eq!(stored.trigger.as_deref(), Some("Rain alerts"));
        let section = registry.available_skills_section();
        assert!(section.starts_with("# Available Skills\n\n"));
        assert!(section.contains("Rain alerts"));
        assert!(section.contains(
            "Load with: read_skill_file(skill_name=\"rain-alerts\", relative_path=\"SKILL.md\")"
        ));
        assert_eq!(section.matches("Load with:").count(), 1);
        assert!(!section.contains("PORTAL_ONLY_TAIL"));
        assert!(!section.contains("BODY_STAYS_ON_DISK"));
        assert!(!section.contains("long description stays"));
        assert!(!section.contains("(instruction)"));
        assert!(!section.contains("when relevant."));
    }

    #[test]
    fn available_skills_truncates_description_when_trigger_absent() {
        let mut registry = SkillRegistry::new();
        let description = format!("{}PORTAL_ONLY_TAIL", "y".repeat(80));
        registry.register(
            make_skill("plain-skill", &description, "BODY_STAYS_ON_DISK", None),
            PathBuf::from("/tmp/test"),
        );
        assert_eq!(
            registry.get("plain-skill").unwrap().description,
            description,
            "portal description stays full"
        );
        let ctx = registry.build_context();
        assert!(!ctx.contains("PORTAL_ONLY_TAIL"));
        assert!(!ctx.contains("BODY_STAYS_ON_DISK"));
        assert_eq!(ctx.matches("Load with:").count(), 1);
        let prose = catalog_prose(&ctx, "plain-skill");
        assert!(
            prose.ends_with('…'),
            "truncated catalog prose was {prose:?}"
        );
        let kept = prose.trim_end_matches('…');
        assert!(
            kept.len() <= CATALOG_TRIGGER_MAX_BYTES,
            "prose bytes {} in {prose:?}",
            kept.len()
        );
        assert!(kept.chars().all(|c| c == 'y'));
    }

    #[test]
    fn blank_trigger_falls_back_to_the_description() {
        let mut registry = SkillRegistry::new();
        let mut skill = make_skill("plain-skill", "Use for notes", "BODY_STAYS_ON_DISK", None);
        skill.trigger = Some("   ".into());
        registry.register(skill, PathBuf::from("/tmp/test"));
        let ctx = registry.build_context();
        assert!(ctx.contains("Use for notes"));
        assert_eq!(ctx.matches("Load with:").count(), 1);
        assert!(!ctx.contains("BODY_STAYS_ON_DISK"));
    }

    #[test]
    fn available_skills_does_not_append_a_second_load_with() {
        let mut registry = SkillRegistry::new();
        let description = "Weather alerts. Load with: read_skill_file(skill_name=\"hko-weather\", relative_path=\"SKILL.md\")";
        registry.register(
            make_skill("hko-weather", description, "BODY_STAYS_ON_DISK", None),
            PathBuf::from("/tmp/test"),
        );
        let ctx = registry.build_context();
        assert_eq!(ctx.matches("Load with:").count(), 1, "{ctx}");
        assert_eq!(ctx.matches("read_skill_file").count(), 2, "{ctx}");
        assert!(ctx.contains("Weather alerts."));
        assert!(!ctx.contains("BODY_STAYS_ON_DISK"));
        assert!(!ctx.contains("when relevant."));
    }

    #[test]
    fn load_with_without_tool_name_is_replaced_by_one_synthetic_clause() {
        let mut registry = SkillRegistry::new();
        registry.register(
            make_skill(
                "weather",
                "Weather alerts. Load with: the skill file please",
                "BODY_STAYS_ON_DISK",
                None,
            ),
            PathBuf::from("/tmp/test"),
        );
        let ctx = registry.build_context();
        assert_eq!(ctx.matches("Load with:").count(), 1, "{ctx}");
        assert!(ctx.contains(
            "Load with: read_skill_file(skill_name=\"weather\", relative_path=\"SKILL.md\")"
        ));
        assert!(!ctx.contains("the skill file"));
        assert!(ctx.contains("Weather alerts."));
    }

    #[test]
    fn trigger_load_with_is_kept_and_description_load_with_is_not_copied() {
        let mut registry = SkillRegistry::new();
        let mut skill = make_skill(
            "alpha",
            "Long portal text. Load with: read_skill_file(skill_name=\"alpha\", relative_path=\"OTHER.md\")",
            "BODY_STAYS_ON_DISK",
            None,
        );
        skill.trigger = Some(
            "Brief. Load with: read_skill_file(skill_name=\"alpha\", relative_path=\"NOTES.md\")"
                .into(),
        );
        registry.register(skill, PathBuf::from("/tmp/test"));
        let ctx = registry.build_context();
        assert_eq!(ctx.matches("Load with:").count(), 1, "{ctx}");
        assert!(ctx.contains("NOTES.md"), "{ctx}");
        assert!(ctx.contains("Brief."));
        assert!(!ctx.contains("OTHER.md"), "{ctx}");
        assert!(!ctx.contains("Long portal text"), "{ctx}");
        assert!(!ctx.contains("BODY_STAYS_ON_DISK"));
    }

    #[test]
    fn trigger_without_load_with_still_gets_one_synthetic_clause() {
        let mut registry = SkillRegistry::new();
        let mut skill = make_skill(
            "alpha",
            "Long portal text. Load with: read_skill_file(skill_name=\"alpha\", relative_path=\"OTHER.md\")",
            "BODY_STAYS_ON_DISK",
            None,
        );
        skill.trigger = Some("Brief trigger".into());
        registry.register(skill, PathBuf::from("/tmp/test"));
        let ctx = registry.build_context();
        assert_eq!(ctx.matches("Load with:").count(), 1, "{ctx}");
        assert!(ctx.contains(
            "Load with: read_skill_file(skill_name=\"alpha\", relative_path=\"SKILL.md\")"
        ));
        assert!(ctx.contains("Brief trigger"));
        assert!(!ctx.contains("OTHER.md"), "{ctx}");
        assert!(!ctx.contains("Long portal text"), "{ctx}");
    }

    #[test]
    fn truncation_keeps_the_author_load_with_clause() {
        let mut registry = SkillRegistry::new();
        let description = format!(
            "{} Load with: read_skill_file(skill_name=\"alpha\", relative_path=\"NOTES.md\")",
            "x".repeat(80)
        );
        registry.register(
            make_skill("alpha", &description, "BODY_STAYS_ON_DISK", None),
            PathBuf::from("/tmp/test"),
        );
        let ctx = registry.build_context();
        assert_eq!(ctx.matches("Load with:").count(), 1, "{ctx}");
        assert!(ctx.contains("NOTES.md"), "{ctx}");
        assert_eq!(ctx.matches("SKILL.md").count(), 1, "{ctx}");
        assert!(!ctx.contains("BODY_STAYS_ON_DISK"));
        let prose = catalog_prose(&ctx, "alpha");
        assert!(prose.ends_with('…'), "{prose:?}");
        assert!(prose.trim_end_matches('…').len() <= CATALOG_TRIGGER_MAX_BYTES);
    }

    #[test]
    fn load_with_on_a_later_description_line_is_kept_once() {
        let mut registry = SkillRegistry::new();
        let description = "This first line is definitely longer than the catalog cap and keeps going.\nLoad with: read_skill_file(skill_name=\"alpha\", relative_path=\"NOTES.md\")";
        registry.register(
            make_skill("alpha", description, "BODY_STAYS_ON_DISK", None),
            PathBuf::from("/tmp/test"),
        );
        let ctx = registry.build_context();
        assert_eq!(ctx.matches("Load with:").count(), 1, "{ctx}");
        assert!(ctx.contains("NOTES.md"), "{ctx}");
        assert_eq!(ctx.matches("SKILL.md").count(), 1, "{ctx}");
        assert!(ctx.contains('…'));
        assert!(!ctx.contains("BODY_STAYS_ON_DISK"));
    }

    #[test]
    fn author_load_with_clause_is_capped() {
        let mut registry = SkillRegistry::new();
        let description = format!(
            "Short. Load with: read_skill_file(skill_name=\"alpha\", relative_path=\"SKILL.md\") {}",
            "z".repeat(400)
        );
        registry.register(
            make_skill("alpha", &description, "BODY_STAYS_ON_DISK", None),
            PathBuf::from("/tmp/test"),
        );
        let ctx = registry.build_context();
        assert_eq!(ctx.matches("Load with:").count(), 1, "{ctx}");
        assert!(ctx.contains("read_skill_file(skill_name=\"alpha\""));
        assert!(ctx.matches('z').count() < 180, "{ctx}");
        assert!(!ctx.contains(&"z".repeat(400)));
        assert!(!ctx.contains("BODY_STAYS_ON_DISK"));
    }

    #[test]
    fn description_with_two_load_with_clauses_emits_only_the_first() {
        let mut registry = SkillRegistry::new();
        registry.register(
            make_skill(
                "alpha",
                "Do X. Load with: read_skill_file(skill_name=\"alpha\", relative_path=\"SKILL.md\") Load with: read_skill_file(skill_name=\"alpha\", relative_path=\"OTHER.md\")",
                "BODY_STAYS_ON_DISK",
                None,
            ),
            PathBuf::from("/tmp/test"),
        );
        let ctx = registry.build_context();
        assert_eq!(ctx.matches("Load with:").count(), 1, "{ctx}");
        assert!(!ctx.contains("OTHER.md"), "{ctx}");
        assert!(ctx.contains("relative_path=\"SKILL.md\""));
    }

    #[test]
    fn catalog_trigger_truncates_on_a_utf8_char_boundary() {
        let mut registry = SkillRegistry::new();
        registry.register(
            make_skill("cjk", &"技".repeat(30), "BODY_STAYS_ON_DISK", None),
            PathBuf::from("/tmp/test"),
        );
        let ctx = registry.build_context();
        let prose = catalog_prose(&ctx, "cjk");
        assert!(prose.ends_with('…'), "{prose:?}");
        let kept = prose.trim_end_matches('…');
        assert_eq!(
            kept.chars().count(),
            CATALOG_TRIGGER_MAX_BYTES / "技".len(),
            "{prose:?}"
        );
        assert!(kept.chars().all(|c| c == '技'));
        assert!(kept.len() <= CATALOG_TRIGGER_MAX_BYTES);
        assert!(!ctx.contains("BODY_STAYS_ON_DISK"));
    }

    #[test]
    fn agent_catalog_keeps_description_cap_and_does_not_add_load_with() {
        let long = format!("Agent does a thing. {}", "z".repeat(300));
        let mut registry = SkillRegistry::new();
        registry.register(
            make_skill("helper", &long, "SECRET_AGENT_BODY", None),
            PathBuf::from("/tmp/test"),
        );
        let agents = registry.build_agents_context();
        assert!(agents.contains("invoke_agent"));
        assert!(!agents.contains("Load with:"));
        assert!(!agents.contains("SECRET_AGENT_BODY"));
        assert!(agents.contains('…'));
        assert!(agents.matches('z').count() > 24);
        assert!(registry.build_context().contains("**helper**"));
    }

    #[test]
    fn available_skills_section_stays_within_12_kib_for_83_long_skills() {
        const STEMS: &[&str] = &[
            "dream-cycle",
            "daily-newspaper",
            "obsidian-inbox-sweep",
            "cantonese-arxiv-daily-briefing",
            "hko-weather",
            "github-pick-to-threads",
            "daily-news-to-threads",
            "daily-devtech-to-threads",
            "awwwards-3d",
            "code-interpreter",
            "creating-skills",
            "memory-manager",
            "news-fetcher",
            "problem-solver",
            "soul-keeper",
            "coding-assistant",
            "vault-github-sync",
            "obsidian-daily-note",
        ];
        let mut registry = SkillRegistry::new();
        for i in 0..83 {
            let name = format!("{}-{:02}", STEMS[i % STEMS.len()], i);
            let description = format!("{}PORTAL_ONLY_TAIL_{i}", "word ".repeat(40));
            let content = format!("BODY_STAYS_ON_DISK_{i}");
            registry.register(
                make_skill(&name, &description, &content, None),
                PathBuf::from("/tmp/test"),
            );
        }
        let section = registry.available_skills_section();
        assert_eq!(CATALOG_TRIGGER_MAX_BYTES, 24);
        assert!(
            section.len() <= AVAILABLE_SKILLS_SECTION_BUDGET_BYTES,
            "Available Skills section is {} bytes (budget {})",
            section.len(),
            AVAILABLE_SKILLS_SECTION_BUDGET_BYTES
        );
        assert_eq!(section.matches("Load with:").count(), 83);
        assert!(!section.contains("PORTAL_ONLY_TAIL"));
        assert!(!section.contains("BODY_STAYS_ON_DISK"));
        assert!(section.contains("read_skill_file"));
        assert!(section.starts_with("# Available Skills\n"));
    }
}
