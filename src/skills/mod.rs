pub mod embed;
pub mod loader;
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

/// A loaded skill from a markdown file
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Skill {
    /// Skill name (derived from filename or frontmatter)
    pub name: String,
    /// Short description
    pub description: String,
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
                instruction_lines.push(format!(
                    "- **{}** (instruction): {}. Load with: read_skill_file(skill_name=\"{}\", relative_path=\"SKILL.md\") when relevant.",
                    skill.name, skill.description, skill.name
                ));
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

    /// Build formatted lines for subagent-style skills (those with model or tools).
    /// Returns formatted lines only (no preamble) — caller prepends the unified section header.
    pub fn build_subagent_lines(&self) -> String {
        let unique_skills = self.list();
        let mut lines = Vec::new();

        for skill in &unique_skills {
            if skill.model.is_some() || !skill.tools.is_empty() {
                lines.push(format!(
                    "- **{}**: {}\n  Invoke via: `invoke_agent(agent=\"{}\", prompt=\"<task>\")`",
                    skill.name, skill.description, skill.name
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
                agent.name, agent.description, agent.name
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
}
