pub mod loader;

use std::collections::HashMap;
use tracing::info;

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
}

/// Registry of all loaded skills
#[derive(Debug, Clone)]
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    /// Register a skill
    pub fn register(&mut self, skill: Skill) {
        info!("Registered skill: {} — {}", skill.name, skill.description);
        self.skills.insert(skill.name.clone(), skill);
    }

    /// Get a skill by name
    #[allow(dead_code)]
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// List all registered skills
    pub fn list(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    /// Build context string for the system prompt.
    /// All skills: metadata only (name + description). Instruction skills get a hint to load
    /// full content via read_skill_file(SKILL.md) when relevant; subagent skills get invoke_subagent hint.
    pub fn build_context(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let mut instruction_lines = String::new();
        let mut subagent_section = String::new();

        for skill in self.skills.values() {
            if skill.model.is_some() {
                // Subagent skill — metadata only + invoke_subagent hint
                subagent_section.push_str(&format!(
                    "- **{}**: {}\n  Invoke via: `invoke_subagent(skill=\"{}\", prompt=\"<task>\")`\n",
                    skill.name, skill.description, skill.name
                ));
            } else {
                // Instruction skill — metadata only + read_skill_file hint (no full body)
                instruction_lines.push_str(&format!(
                    "- **{}** (instruction): {}. Load with: read_skill_file(skill_name=\"{}\", relative_path=\"SKILL.md\") when relevant.\n",
                    skill.name, skill.description, skill.name
                ));
            }
        }

        let mut context = String::new();

        if !instruction_lines.is_empty() {
            context.push_str(
                "When an instruction skill is relevant, load its full instructions with read_skill_file(skill_name=\"<name>\", relative_path=\"SKILL.md\"), then follow them. For subagent skills use invoke_subagent.\n\n",
            );
            context.push_str("You have the following skills available:\n\n");
            context.push_str(&instruction_lines);
        }

        if !subagent_section.is_empty() {
            if !instruction_lines.is_empty() {
                context.push('\n');
            }
            context.push_str("## Available Subagent Skills\n\n");
            context.push_str("Delegate these tasks using `invoke_subagent`:\n\n");
            context.push_str(&subagent_section);
        }

        context
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

    fn make_skill(name: &str, description: &str, content: &str, model: Option<&str>) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            content: content.to_string(),
            tags: vec![],
            model: model.map(str::to_string),
            tools: vec![],
            max_iterations: None,
        }
    }

    #[test]
    fn test_build_context_instruction_skill_metadata_only() {
        // Instruction skills: metadata only + read_skill_file hint; full body NOT injected
        let mut registry = SkillRegistry::new();
        registry.register(make_skill(
            "my-skill",
            "Does things",
            "# Instructions\nDo this and that.",
            None, // no model = instruction skill
        ));
        let ctx = registry.build_context();
        assert!(ctx.contains("my-skill"));
        assert!(ctx.contains("Does things"));
        assert!(ctx.contains("read_skill_file"));
        assert!(ctx.contains("SKILL.md"));
        assert!(!ctx.contains("# Instructions"));
        assert!(!ctx.contains("Do this and that."));
    }

    #[test]
    fn test_build_context_subagent_skill_injects_metadata_only() {
        // Skills with a model field get only name + description + invoke hint
        let mut registry = SkillRegistry::new();
        registry.register(make_skill(
            "thread-writer",
            "Use when writing Thread posts.",
            "# Super Secret Instructions\nLong style guide...",
            Some("anthropic/claude-sonnet-4-6"),
        ));
        let ctx = registry.build_context();
        // Metadata present
        assert!(ctx.contains("thread-writer"));
        assert!(ctx.contains("Use when writing Thread posts."));
        assert!(ctx.contains("invoke_subagent"));
        // Body NOT present
        assert!(!ctx.contains("Super Secret Instructions"));
        assert!(!ctx.contains("Long style guide"));
    }

    #[test]
    fn test_build_context_empty_registry() {
        let registry = SkillRegistry::new();
        assert_eq!(registry.build_context(), String::new());
    }

    #[test]
    fn test_build_context_mixed_skills() {
        // Both skill types: metadata only; instruction body and subagent body NOT in context
        let mut registry = SkillRegistry::new();
        registry.register(make_skill(
            "instruction-skill",
            "An instruction skill",
            "Follow these instructions.",
            None,
        ));
        registry.register(make_skill(
            "subagent-skill",
            "A subagent skill",
            "Secret subagent body.",
            Some("some/model"),
        ));
        let ctx = registry.build_context();
        assert!(ctx.contains("instruction-skill"));
        assert!(ctx.contains("An instruction skill"));
        assert!(ctx.contains("read_skill_file"));
        assert!(!ctx.contains("Follow these instructions."));
        assert!(!ctx.contains("Secret subagent body."));
        assert!(ctx.contains("invoke_subagent"));
        assert!(ctx.contains("You have the following skills available"));
        assert!(ctx.contains("Available Subagent Skills"));
    }
}
