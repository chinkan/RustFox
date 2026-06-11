use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use super::{Skill, SkillRegistry, SkillSource};

/// Load all markdown skill files from a directory.
///
/// Supports two formats:
/// 1. `skills/my-skill.md` — standalone markdown file
/// 2. `skills/my-skill/SKILL.md` — directory with a SKILL.md file
///
/// Skill files can have optional YAML frontmatter:
/// ```markdown
/// ---
/// name: my-skill
/// description: What this skill does
/// tags: [coding, review]
/// ---
/// # Instructions here...
/// ```
pub async fn load_skills_from_dir(
    dir: &Path,
    source: SkillSource,
    base_dir: PathBuf,
) -> Result<SkillRegistry> {
    let mut registry = SkillRegistry::new();

    if !dir.exists() {
        info!("Skills directory not found: {}, skipping", dir.display());
        return Ok(registry);
    }

    let mut entries = tokio::fs::read_dir(dir)
        .await
        .with_context(|| format!("Failed to read skills directory: {}", dir.display()))?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        // Support .md files and directories containing SKILL.md or AGENT.md
        let skill_path = if path.is_dir() {
            let skill_file = path.join("SKILL.md");
            let agent_file = path.join("AGENT.md");
            if skill_file.exists() {
                skill_file
            } else if agent_file.exists() {
                agent_file
            } else {
                continue;
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            path.clone()
        } else {
            continue;
        };

        match load_skill_file(&skill_path).await {
            Ok(skill) => registry.register(skill, source, base_dir.clone()),
            Err(e) => warn!("Failed to load skill from {}: {}", skill_path.display(), e),
        }
    }

    info!("Loaded {} skills", registry.len());
    Ok(registry)
}

async fn load_skill_file(path: &Path) -> Result<Skill> {
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read skill file: {}", path.display()))?;

    // Try to parse YAML frontmatter
    if let Some(stripped) = content.strip_prefix("---") {
        if let Some(end) = stripped.find("---") {
            let frontmatter = stripped[..end].trim();
            let body = stripped[end + 3..].trim().to_string();

            let name = extract_field(frontmatter, "name");
            let description = extract_field(frontmatter, "description");
            let tags = extract_list_field(frontmatter, "tags");

            let skill_name = name.unwrap_or_else(|| name_from_path(path));

            return Ok(Skill {
                name: skill_name,
                description: description.unwrap_or_else(|| first_line_or_heading(&body)),
                content: body,
                tags,
                model: extract_field(frontmatter, "model"),
                tools: extract_list_field(frontmatter, "tools"),
                max_iterations: extract_u32_field(frontmatter, "max_iterations"),
                skip_bootstrap: extract_bool_field(frontmatter, "skip_bootstrap"),
                supervisor_workflow: extract_nested_field(frontmatter, "supervisor", "workflow"),
                supervisor_required_caps: extract_nested_list(
                    frontmatter,
                    "supervisor",
                    "required_capabilities",
                ),
            });
        }
    }

    // No frontmatter — derive metadata from content
    let name = name_from_path(path);
    let description = first_line_or_heading(&content);

    Ok(Skill {
        name,
        description,
        content: content.to_string(),
        tags: Vec::new(),
        model: None,
        tools: vec![],
        max_iterations: None,
        skip_bootstrap: false,
        supervisor_workflow: None,
        supervisor_required_caps: vec![],
    })
}

/// Extract a simple `key: value` from YAML-like frontmatter
fn extract_field(frontmatter: &str, key: &str) -> Option<String> {
    let prefix = format!("{}:", key);
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Extract a simple `key: [a, b, c]` list from frontmatter
fn extract_list_field(frontmatter: &str, key: &str) -> Vec<String> {
    let prefix = format!("{}:", key);
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            let rest = rest.trim();
            if rest.starts_with('[') && rest.ends_with(']') {
                return rest[1..rest.len() - 1]
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Extract a `key: N` unsigned integer field from YAML-like frontmatter
fn extract_u32_field(frontmatter: &str, key: &str) -> Option<u32> {
    extract_field(frontmatter, key)?.parse().ok()
}

/// Parse a boolean field from frontmatter text.
fn extract_bool_field(frontmatter: &str, key: &str) -> bool {
    extract_field(frontmatter, key)
        .map(|v| v == "true" || v == "yes" || v == "1")
        .unwrap_or(false)
}

/// Extract `parent.subkey: value` from a YAML-like block where the parent has its
/// own line followed by 2-space-indented sub-keys.
fn extract_nested_field(frontmatter: &str, parent: &str, subkey: &str) -> Option<String> {
    let parent_prefix = format!("{}:", parent);
    let sub_prefix = format!("{}:", subkey);
    let mut in_block = false;
    for line in frontmatter.lines() {
        let stripped = line.trim_start();
        if stripped == parent_prefix.as_str()
            || stripped.starts_with(&format!("{} ", parent_prefix))
        {
            in_block = true;
            continue;
        }
        if in_block {
            if !line.starts_with(' ') && !line.starts_with('\t') && !line.is_empty() {
                in_block = false;
                continue;
            }
            let inner = line.trim_start();
            if let Some(rest) = inner.strip_prefix(&sub_prefix) {
                let value = rest.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn extract_nested_list(frontmatter: &str, parent: &str, subkey: &str) -> Vec<String> {
    let raw = match extract_nested_field(frontmatter, parent, subkey) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let raw = raw.trim();
    if raw.starts_with('[') && raw.ends_with(']') {
        raw[1..raw.len() - 1]
            .split(',')
            .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    }
}

/// Derive skill/agent name from file path
fn name_from_path(path: &Path) -> String {
    // If it's SKILL.md or AGENT.md inside a directory, use the directory name
    let filename = path.file_name().and_then(|f| f.to_str());
    if matches!(filename, Some("SKILL.md") | Some("AGENT.md")) {
        if let Some(parent) = path.parent() {
            if let Some(dir_name) = parent.file_name().and_then(|f| f.to_str()) {
                return dir_name.to_string();
            }
        }
    }
    // Otherwise use the file stem
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string()
}

/// Get the first heading or first line as a description
fn first_line_or_heading(content: &str) -> String {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(heading) = line.strip_prefix('#') {
            return heading.trim().trim_start_matches('#').trim().to_string();
        }
        return line.to_string();
    }
    "No description".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_u32_field_present() {
        let fm = "name: my-skill\nmax_iterations: 8\n";
        assert_eq!(extract_u32_field(fm, "max_iterations"), Some(8));
    }

    #[test]
    fn test_extract_u32_field_absent() {
        let fm = "name: my-skill\n";
        assert_eq!(extract_u32_field(fm, "max_iterations"), None);
    }

    #[test]
    fn test_extract_u32_field_invalid_value() {
        let fm = "max_iterations: not-a-number\n";
        assert_eq!(extract_u32_field(fm, "max_iterations"), None);
    }

    #[test]
    fn test_load_skill_parses_model_field() {
        let frontmatter =
            "name: thread-writer\ndescription: Write posts\nmodel: anthropic/claude-sonnet-4-6\n";
        let model = extract_field(frontmatter, "model");
        assert_eq!(model.as_deref(), Some("anthropic/claude-sonnet-4-6"));
    }

    #[test]
    fn test_load_skill_parses_tools_field() {
        let frontmatter = "tools: [read_skill_file, mcp_threads_post]\n";
        let tools = extract_list_field(frontmatter, "tools");
        assert_eq!(tools, vec!["read_skill_file", "mcp_threads_post"]);
    }

    #[test]
    fn test_load_skill_defaults_when_fields_absent() {
        let frontmatter = "name: plain-skill\ndescription: Simple skill\n";
        assert_eq!(extract_field(frontmatter, "model"), None);
        assert!(extract_list_field(frontmatter, "tools").is_empty());
        assert_eq!(extract_u32_field(frontmatter, "max_iterations"), None);
    }

    #[test]
    fn extract_nested_field_finds_subkey() {
        let fm = "name: x\nsupervisor:\n  workflow: research\n  required_capabilities: [a, b]\n";
        assert_eq!(
            extract_nested_field(fm, "supervisor", "workflow").as_deref(),
            Some("research")
        );
        assert_eq!(
            extract_nested_list(fm, "supervisor", "required_capabilities"),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[tokio::test]
    async fn skill_with_supervisor_block_loads_workflow_hint() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("research-pack");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: research-pack\ndescription: research workflow\n\
             supervisor:\n  workflow: research\n  required_capabilities: [research]\n---\nbody",
        )
        .await
        .unwrap();
        let skills =
            load_skills_from_dir(dir.path(), SkillSource::Instance, dir.path().to_path_buf())
                .await
                .unwrap();
        let s = skills.get("research-pack").unwrap();
        assert_eq!(s.supervisor_workflow.as_deref(), Some("research"));
        assert_eq!(s.supervisor_required_caps, vec!["research".to_string()]);
    }
}
