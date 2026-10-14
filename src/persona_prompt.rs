//! Per-bot persona prompt binding (design §7.4).
//!
//! Each `[[bots]]` entry resolves its own base system prompt and SOUL overlay
//! from `agents/<persona>/` (AGENT.md + SOUL.md). **AGENTS.md (learned memory)
//! and USER.md stay install-wide / process-wide** (PO/TL lock) — never read
//! `agents/<persona>/AGENTS.md`. Portal / scheduler keep the shim/default
//! persona in v1.

use std::path::{Path, PathBuf};

use crate::config::{BotConfig, Config, SystemPromptSource};

/// Where the bot's base system prompt text came from.
#[derive(Debug, Clone, PartialEq)]
pub enum BotPromptSource {
    /// `[[bots]].system_prompt_file` (highest precedence when readable).
    BotFile(PathBuf),
    /// Body of `agents/<persona>/AGENT.md`.
    PersonaAgent(PathBuf),
    /// Install-wide `[openrouter]` resolve_system_prompt (backward compat).
    Global(SystemPromptSource),
}

/// Resolved soul-file paths for a bot persona.
///
/// `agents_md` and `user` are **always** the shared home copies (PO/TL lock).
/// SOUL prefers a persona overlay under `agents/<persona>/` when the file exists.
#[derive(Debug, Clone, PartialEq)]
pub struct PersonaSoulFiles {
    pub soul: PathBuf,
    pub agents_md: PathBuf,
    pub user: PathBuf,
    pub soul_from_persona: bool,
}

impl PersonaSoulFiles {
    /// Home-only paths (subagents / no persona overlay).
    pub fn home_only(home: &Path) -> Self {
        Self {
            soul: home.join("SOUL.md"),
            agents_md: home.join("AGENTS.md"),
            user: home.join("USER.md"),
            soul_from_persona: false,
        }
    }
}

/// Strip a leading YAML frontmatter block (`--- … ---`) if present.
pub fn strip_md_frontmatter(content: &str) -> String {
    let trimmed = content.trim_start();
    if let Some(after_first) = trimmed.strip_prefix("---") {
        if let Some(end_pos) = after_first.find("---") {
            return after_first[end_pos + 3..].trim_start().to_string();
        }
    }
    content.to_string()
}

/// Look up the bot used for prompt binding.
///
/// Prefer an exact `bots[].id` match (after normalize). If missing (e.g. portal
/// still uses `"default"` while config only has `id = "main"`), fall back to
/// [`Config::shim_bot`] so the portal keeps the single/default persona in v1.
pub fn bot_for_prompt<'a>(config: &'a Config, bot_id: &str) -> &'a BotConfig {
    let id = crate::platform::normalize_bot_id(bot_id);
    config
        .bots
        .iter()
        .find(|b| b.id.trim() == id)
        .unwrap_or_else(|| Config::shim_bot(&config.bots))
}

/// Resolve the base system prompt for a bot (§7.4).
///
/// Precedence:
/// 1. `bot.system_prompt_file` (if set and non-empty on disk)
/// 2. `agents/<persona>/AGENT.md` body (frontmatter stripped)
/// 3. Global [`Config::resolve_system_prompt`] (file > inline > builtin)
pub fn resolve_bot_base_prompt(config: &Config, bot: &BotConfig) -> (String, BotPromptSource) {
    if let Some(ptr) = bot
        .system_prompt_file
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty())
    {
        let path = config.resolve_prompt_path(ptr);
        match std::fs::read_to_string(&path) {
            Ok(content) if !content.trim().is_empty() => {
                return (
                    strip_md_frontmatter(&content),
                    BotPromptSource::BotFile(path),
                );
            }
            Ok(_) => {
                tracing::warn!(
                    bot_id = %bot.id,
                    path = %path.display(),
                    "bots[].system_prompt_file is empty — falling back to persona AGENT.md / global prompt"
                );
            }
            Err(e) => {
                tracing::warn!(
                    bot_id = %bot.id,
                    path = %path.display(),
                    error = %e,
                    "bots[].system_prompt_file unreadable — falling back to persona AGENT.md / global prompt"
                );
            }
        }
    }

    let agent_md = config
        .agents
        .directory
        .join(bot.persona.trim())
        .join("AGENT.md");
    if agent_md.is_file() {
        match std::fs::read_to_string(&agent_md) {
            Ok(content) if !content.trim().is_empty() => {
                return (
                    strip_md_frontmatter(&content),
                    BotPromptSource::PersonaAgent(agent_md),
                );
            }
            Ok(_) => {
                tracing::warn!(
                    bot_id = %bot.id,
                    persona = %bot.persona,
                    path = %agent_md.display(),
                    "agents/<persona>/AGENT.md is empty — falling back to global system prompt"
                );
            }
            Err(e) => {
                tracing::warn!(
                    bot_id = %bot.id,
                    persona = %bot.persona,
                    path = %agent_md.display(),
                    error = %e,
                    "agents/<persona>/AGENT.md unreadable — falling back to global system prompt"
                );
            }
        }
    }

    let (prompt, source) = config.resolve_system_prompt();
    (prompt, BotPromptSource::Global(source))
}

/// Resolve SOUL / AGENTS.md / USER.md paths for a persona.
///
/// AGENTS.md and USER.md are always `home/…` (shared learned memory / user
/// model — PO/TL lock). SOUL uses `agents/<persona>/SOUL.md` when present,
/// else the home copy. Persona bind (§7.4) is AGENT.md + SOUL only.
pub fn resolve_persona_soul_files(
    home: &Path,
    agents_dir: &Path,
    persona: &str,
) -> PersonaSoulFiles {
    let persona = persona.trim();
    let persona_dir = agents_dir.join(persona);

    let persona_soul = persona_dir.join("SOUL.md");
    let (soul, soul_from_persona) = if !persona.is_empty() && persona_soul.is_file() {
        (persona_soul, true)
    } else {
        (home.join("SOUL.md"), false)
    };

    PersonaSoulFiles {
        soul,
        agents_md: home.join("AGENTS.md"),
        user: home.join("USER.md"),
        soul_from_persona,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg_with_home_and_bots(home: &Path, bots_toml: &str) -> Config {
        let mut cfg: Config = toml::from_str(&format!(
            r#"
            {bots_toml}
            [openrouter]
            api_key = "key"
            system_prompt = "GLOBAL_INLINE_PROMPT"
            [general]
            home = '{home}'
            "#,
            bots_toml = bots_toml,
            home = home.display()
        ))
        .unwrap();
        cfg.normalize_bots().unwrap();
        cfg.resolve().unwrap();
        cfg
    }

    #[test]
    fn strip_md_frontmatter_removes_yaml_block() {
        let raw = "---\nname: researcher\n---\n\nYou are Researcher.\n";
        assert_eq!(strip_md_frontmatter(raw), "You are Researcher.\n");
        assert_eq!(strip_md_frontmatter("plain body"), "plain body");
    }

    #[test]
    fn different_personas_resolve_different_agent_prompts() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let agents = home.join("agents");
        std::fs::create_dir_all(agents.join("main")).unwrap();
        std::fs::create_dir_all(agents.join("researcher")).unwrap();
        std::fs::write(
            agents.join("main/AGENT.md"),
            "---\nname: main\n---\nMAIN_PERSONA_BODY\n",
        )
        .unwrap();
        std::fs::write(
            agents.join("researcher/AGENT.md"),
            "---\nname: researcher\n---\nRESEARCHER_PERSONA_BODY\n",
        )
        .unwrap();
        // Shared USER.md
        std::fs::write(home.join("USER.md"), "SHARED_USER_MODEL\n").unwrap();
        std::fs::write(agents.join("main/SOUL.md"), "MAIN_SOUL\n").unwrap();
        std::fs::write(agents.join("researcher/SOUL.md"), "RESEARCHER_SOUL\n").unwrap();

        let cfg = cfg_with_home_and_bots(
            &home,
            r#"
            [[bots]]
            id = "main"
            bot_token = "tok-main"
            allowed_user_ids = [1]
            persona = "main"

            [[bots]]
            id = "researcher"
            bot_token = "tok-researcher"
            allowed_user_ids = [1]
            persona = "researcher"
            "#,
        );

        let main_bot = bot_for_prompt(&cfg, "main");
        let research_bot = bot_for_prompt(&cfg, "researcher");
        let (main_prompt, main_src) = resolve_bot_base_prompt(&cfg, main_bot);
        let (research_prompt, research_src) = resolve_bot_base_prompt(&cfg, research_bot);

        assert!(
            main_prompt.contains("MAIN_PERSONA_BODY"),
            "main prompt: {main_prompt}"
        );
        assert!(
            research_prompt.contains("RESEARCHER_PERSONA_BODY"),
            "researcher prompt: {research_prompt}"
        );
        assert_ne!(main_prompt, research_prompt);
        assert!(matches!(main_src, BotPromptSource::PersonaAgent(_)));
        assert!(matches!(research_src, BotPromptSource::PersonaAgent(_)));

        let main_soul = resolve_persona_soul_files(&home, &cfg.agents.directory, "main");
        let research_soul = resolve_persona_soul_files(&home, &cfg.agents.directory, "researcher");
        assert!(main_soul.soul_from_persona);
        assert!(research_soul.soul_from_persona);
        assert_ne!(main_soul.soul, research_soul.soul);
        // USER.md shared
        assert_eq!(main_soul.user, research_soul.user);
        assert_eq!(main_soul.user, home.join("USER.md"));
        assert!(!main_soul.user.starts_with(&cfg.agents.directory));
        // AGENTS.md always home (learned memory) — even if personas differ
        assert_eq!(main_soul.agents_md, research_soul.agents_md);
        assert_eq!(main_soul.agents_md, home.join("AGENTS.md"));
    }

    #[test]
    fn shared_user_md_never_splits_per_persona() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let agents = home.join("agents");
        std::fs::create_dir_all(agents.join("alpha")).unwrap();
        std::fs::create_dir_all(agents.join("beta")).unwrap();
        // Even if a mistaken per-persona USER.md exists, resolution ignores it.
        std::fs::write(agents.join("alpha/USER.md"), "ALPHA_USER\n").unwrap();
        std::fs::write(home.join("USER.md"), "SHARED\n").unwrap();

        let alpha = resolve_persona_soul_files(&home, &agents, "alpha");
        let beta = resolve_persona_soul_files(&home, &agents, "beta");
        assert_eq!(alpha.user, home.join("USER.md"));
        assert_eq!(beta.user, home.join("USER.md"));
        assert_eq!(alpha.user, beta.user);
    }

    #[test]
    fn bot_system_prompt_file_wins_over_persona_agent_md() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let agents = home.join("agents");
        std::fs::create_dir_all(agents.join("main")).unwrap();
        std::fs::write(agents.join("main/AGENT.md"), "FROM_AGENT_MD\n").unwrap();
        std::fs::create_dir_all(home.join("prompts")).unwrap();
        std::fs::write(home.join("prompts/bot.md"), "FROM_BOT_FILE\n").unwrap();

        let cfg = cfg_with_home_and_bots(
            &home,
            r#"
            [[bots]]
            id = "main"
            bot_token = "tok"
            allowed_user_ids = [1]
            persona = "main"
            system_prompt_file = "prompts/bot.md"
            "#,
        );
        let bot = bot_for_prompt(&cfg, "main");
        let (prompt, src) = resolve_bot_base_prompt(&cfg, bot);
        assert_eq!(prompt.trim(), "FROM_BOT_FILE");
        assert!(matches!(src, BotPromptSource::BotFile(_)));
    }

    #[test]
    fn missing_persona_pack_falls_back_to_global_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        std::fs::create_dir_all(&home).unwrap();

        let cfg = cfg_with_home_and_bots(
            &home,
            r#"
            [[bots]]
            id = "default"
            bot_token = "tok"
            allowed_user_ids = [1]
            persona = "main"
            "#,
        );
        let bot = bot_for_prompt(&cfg, "default");
        let (prompt, src) = resolve_bot_base_prompt(&cfg, bot);
        assert_eq!(prompt, "GLOBAL_INLINE_PROMPT");
        assert!(matches!(src, BotPromptSource::Global(_)));
    }

    #[test]
    fn portal_default_bot_id_uses_shim_when_only_main_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let agents = home.join("agents");
        std::fs::create_dir_all(agents.join("main")).unwrap();
        std::fs::write(agents.join("main/AGENT.md"), "SHIM_MAIN\n").unwrap();

        let cfg = cfg_with_home_and_bots(
            &home,
            r#"
            [[bots]]
            id = "main"
            bot_token = "tok"
            allowed_user_ids = [1]
            persona = "main"
            "#,
        );
        // Portal still passes bot_id = "default"
        let bot = bot_for_prompt(&cfg, crate::platform::DEFAULT_BOT_ID);
        assert_eq!(bot.id, "main");
        let (prompt, _) = resolve_bot_base_prompt(&cfg, bot);
        assert!(prompt.contains("SHIM_MAIN"));
    }

    #[test]
    fn soul_overlay_falls_back_to_home_when_persona_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let agents = home.join("agents");
        std::fs::create_dir_all(agents.join("researcher")).unwrap();
        std::fs::write(home.join("SOUL.md"), "HOME_SOUL\n").unwrap();
        std::fs::write(home.join("AGENTS.md"), "HOME_AGENTS\n").unwrap();
        std::fs::write(home.join("USER.md"), "HOME_USER\n").unwrap();

        let files = resolve_persona_soul_files(&home, &agents, "researcher");
        assert!(!files.soul_from_persona);
        assert_eq!(files.soul, home.join("SOUL.md"));
        assert_eq!(files.agents_md, home.join("AGENTS.md"));
        assert_eq!(files.user, home.join("USER.md"));
    }

    #[test]
    fn agents_md_always_home_even_when_persona_agents_md_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let agents = home.join("agents");
        std::fs::create_dir_all(agents.join("researcher")).unwrap();
        std::fs::write(home.join("AGENTS.md"), "HOME_LEARNED_MEMORY\n").unwrap();
        // Mistaken per-persona AGENTS.md must be ignored (PO/TL lock §7.4).
        std::fs::write(
            agents.join("researcher/AGENTS.md"),
            "PERSONA_AGENTS_MUST_NOT_WIN\n",
        )
        .unwrap();
        std::fs::write(agents.join("researcher/SOUL.md"), "RESEARCHER_SOUL\n").unwrap();
        std::fs::write(home.join("USER.md"), "SHARED_USER\n").unwrap();

        let files = resolve_persona_soul_files(&home, &agents, "researcher");
        assert_eq!(files.agents_md, home.join("AGENTS.md"));
        assert!(!files.agents_md.starts_with(&agents));
        assert!(files.soul_from_persona);
        assert_eq!(files.soul, agents.join("researcher/SOUL.md"));
        assert_eq!(files.user, home.join("USER.md"));
    }

    #[test]
    fn different_personas_share_home_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".rustfox");
        let agents = home.join("agents");
        std::fs::create_dir_all(agents.join("main")).unwrap();
        std::fs::create_dir_all(agents.join("researcher")).unwrap();
        std::fs::write(home.join("AGENTS.md"), "SHARED_MEMORY\n").unwrap();
        std::fs::write(agents.join("main/AGENTS.md"), "MAIN_AGENTS\n").unwrap();
        std::fs::write(agents.join("researcher/AGENTS.md"), "RESEARCHER_AGENTS\n").unwrap();
        std::fs::write(agents.join("main/SOUL.md"), "MAIN_SOUL\n").unwrap();
        std::fs::write(agents.join("researcher/SOUL.md"), "RESEARCHER_SOUL\n").unwrap();

        let main = resolve_persona_soul_files(&home, &agents, "main");
        let research = resolve_persona_soul_files(&home, &agents, "researcher");
        assert_eq!(main.agents_md, research.agents_md);
        assert_eq!(main.agents_md, home.join("AGENTS.md"));
        assert_ne!(main.soul, research.soul);
        assert!(main.soul_from_persona);
        assert!(research.soul_from_persona);
    }
}
