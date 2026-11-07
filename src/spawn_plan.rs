//! Plan `spawn_agents` tasks (ADR-0062 / #375).
//!
//! A task with a resolvable `agent` or `name` loads that pack the same way as
//! `invoke_agent`. An unknown name is an error. Omitting both keeps ad-hoc spawn.

use serde_json::Value;

use crate::config::BotConfig;
use crate::peer_invoke::InvokeSource;
use crate::skills::SkillRegistry;

/// One validated `spawn_agents` task, before registry resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnTask {
    pub agent_name: Option<String>,
    pub system_prompt: Option<String>,
    pub prompt: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
}

/// How a planned task will run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpawnKind {
    /// No `agent` / `name`. Today's ad-hoc tool set.
    AdHoc,
    /// Resolved pack. `stack_key` is the last frame of `child_stack`, the same
    /// key `invoke_agent` would push.
    Named { name: String, source: InvokeSource },
}

/// One child the spawn handler can launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedSpawn {
    pub kind: SpawnKind,
    pub system_prompt: String,
    pub prompt: String,
    pub model: Option<String>,
    pub tools_override: Option<Vec<String>>,
    pub allowed_tools: Vec<String>,
    pub child_stack: Vec<String>,
}

impl PlannedSpawn {
    /// Registry name passed to `run_subagent`. `None` keeps ad-hoc mode.
    pub(crate) fn skill_name(&self) -> Option<&str> {
        match &self.kind {
            SpawnKind::AdHoc => None,
            SpawnKind::Named { name, .. } => Some(name.as_str()),
        }
    }

    /// Peer delivery source. Ad-hoc stays on the caller.
    pub(crate) fn invoke_source(&self) -> Option<&InvokeSource> {
        match &self.kind {
            SpawnKind::AdHoc => None,
            SpawnKind::Named { source, .. } => Some(source),
        }
    }
}

/// Registries and ad-hoc defaults the planner consults.
pub(crate) struct SpawnCatalog<'a> {
    pub agents: &'a SkillRegistry,
    pub skills: &'a SkillRegistry,
    pub bots: &'a [BotConfig],
    /// `[subagents].default_tools`. `None` selects the builtin four-tool set.
    pub adhoc_default_tools: Option<&'a [String]>,
}

/// Builtin ad-hoc sandbox tools when `[subagents].default_tools` is unset.
pub(crate) fn builtin_adhoc_tools() -> Vec<String> {
    ["read_file", "write_file", "list_files", "execute_command"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Tool whitelist for a named pack, same formula as `invoke_agent`.
pub(crate) fn named_subagent_tools(
    parent_tools: Option<&[String]>,
    tools_override: Option<&[String]>,
    declared: &[String],
) -> Result<Vec<String>, String> {
    let resolved_today = match tools_override {
        Some(over) => over.to_vec(),
        None => declared.to_vec(),
    };
    let declared_for_cap = if declared.is_empty() {
        None
    } else {
        Some(declared)
    };
    crate::agent::resolve_capped_subagent_tools(
        parent_tools,
        tools_override,
        &resolved_today,
        declared_for_cap,
        true,
    )
}

/// Tool whitelist for an ad-hoc spawn (no pack, no read-tool inject).
pub(crate) fn adhoc_subagent_tools(
    parent_tools: Option<&[String]>,
    tools_override: Option<&[String]>,
    defaults: &[String],
) -> Result<Vec<String>, String> {
    let resolved_today = match tools_override {
        Some(over) => over.to_vec(),
        None => defaults.to_vec(),
    };
    crate::agent::resolve_capped_subagent_tools(
        parent_tools,
        tools_override,
        &resolved_today,
        None,
        false,
    )
}

/// Resolve every task in a `spawn_agents` call. Any error fails the whole call
/// before a child starts.
pub(crate) fn plan_spawn_agents(
    args: &Value,
    parent_stack: &[String],
    parent_tools: Option<&[String]>,
    catalog: &SpawnCatalog<'_>,
) -> Result<Vec<PlannedSpawn>, String> {
    let in_batch = args.get("tasks").and_then(Value::as_array).is_some();
    let tasks = parse_spawn_tasks(args)?;
    let defaults = catalog
        .adhoc_default_tools
        .map(|tools| tools.to_vec())
        .unwrap_or_else(builtin_adhoc_tools);
    let mut plans = Vec::with_capacity(tasks.len());
    for (index, mut task) in tasks.into_iter().enumerate() {
        let agent_name = task.agent_name.take();
        let planned = match agent_name {
            None => plan_adhoc_task(task, index, parent_stack, parent_tools, &defaults)?,
            Some(name) => plan_named_task(
                name,
                task,
                index,
                in_batch,
                parent_stack,
                parent_tools,
                catalog,
            )?,
        };
        plans.push(planned);
    }
    Ok(plans)
}

fn plan_adhoc_task(
    task: SpawnTask,
    index: usize,
    parent_stack: &[String],
    parent_tools: Option<&[String]>,
    defaults: &[String],
) -> Result<PlannedSpawn, String> {
    let child_stack = crate::peer_invoke::spawn_agents_child_stack(parent_stack, index)?;
    let allowed_tools = adhoc_subagent_tools(parent_tools, task.tools.as_deref(), defaults)?;
    Ok(PlannedSpawn {
        kind: SpawnKind::AdHoc,
        system_prompt: task.system_prompt.unwrap_or_default(),
        prompt: task.prompt,
        model: task.model,
        tools_override: task.tools,
        allowed_tools,
        child_stack,
    })
}

fn plan_named_task(
    name: String,
    task: SpawnTask,
    index: usize,
    in_batch: bool,
    parent_stack: &[String],
    parent_tools: Option<&[String]>,
    catalog: &SpawnCatalog<'_>,
) -> Result<PlannedSpawn, String> {
    let (source, declared) =
        resolve_spawn_target(&name, catalog).map_err(|err| task_error(index, in_batch, err))?;
    let preliminary = crate::peer_invoke::stack_key_for_invoke(&name, &Some(source.clone()));
    let stack_key = crate::peer_invoke::canonicalize_self_stack_key(
        parent_stack,
        &name,
        &preliminary,
        catalog.bots,
    );
    // Cycle and depth errors stay unprefixed so they hard-abort like invoke_agent.
    crate::peer_invoke::guard_peer_invoke(parent_stack, &stack_key)?;
    let child_stack = crate::peer_invoke::push_invoke_stack(parent_stack, &stack_key);
    let allowed_tools = named_subagent_tools(parent_tools, task.tools.as_deref(), &declared)
        .map_err(|err| task_error(index, in_batch, err))?;
    Ok(PlannedSpawn {
        kind: SpawnKind::Named { name, source },
        // The pack is the system prompt. A caller system_prompt does not replace it.
        system_prompt: String::new(),
        prompt: task.prompt,
        model: task.model,
        tools_override: task.tools,
        allowed_tools,
        child_stack,
    })
}

/// Agents → skills → bots, matching `invoke_agent`. Declared tools follow
/// `run_subagent`: a pack on disk wins over `bots[].tools`.
fn resolve_spawn_target(
    name: &str,
    catalog: &SpawnCatalog<'_>,
) -> Result<(InvokeSource, Vec<String>), String> {
    let skip_pack = crate::peer_invoke::bot_id_is_unmapped_pack_name(catalog.bots, name);
    let in_agents = !skip_pack && catalog.agents.get(name).is_some();
    let in_skills = !skip_pack && !in_agents && catalog.skills.get(name).is_some();
    let Some(source) =
        crate::peer_invoke::resolve_invoke_source(name, in_agents, in_skills, catalog.bots)
    else {
        return Err(format!(
            "Unknown agent '{name}'. spawn_agents with agent/name loads that pack and does not fall back to ad-hoc tools."
        ));
    };
    let declared = if let Some(skill) = catalog.agents.get(name) {
        skill.tools.clone()
    } else if let Some(skill) = catalog.skills.get(name) {
        skill.tools.clone()
    } else {
        crate::peer_invoke::bot_config_for_peer(catalog.bots, name)
            .and_then(|bot| bot.tools.clone())
            .unwrap_or_default()
    };
    Ok((source, declared))
}

fn task_error(index: usize, in_batch: bool, err: String) -> String {
    if !in_batch || crate::peer_invoke::is_hard_invoke_error(&err) {
        err
    } else {
        format!("Task at index {index}: {err}")
    }
}

fn parse_spawn_tasks(args: &Value) -> Result<Vec<SpawnTask>, String> {
    if let Some(tasks) = args.get("tasks").and_then(Value::as_array) {
        if tasks.is_empty() {
            return Err("tasks array is empty".to_string());
        }
        let mut parsed = Vec::with_capacity(tasks.len());
        for (index, task) in tasks.iter().enumerate() {
            parsed.push(parse_spawn_task(task, Some(index))?);
        }
        return Ok(parsed);
    }
    Ok(vec![parse_spawn_task(args, None)?])
}

fn parse_spawn_task(task: &Value, index: Option<usize>) -> Result<SpawnTask, String> {
    let agent_name = spawn_agent_name(task);
    let system_prompt = task
        .get("system_prompt")
        .and_then(Value::as_str)
        .map(str::to_string);
    let prompt = match task.get("prompt").and_then(Value::as_str) {
        Some(prompt) => prompt.to_string(),
        None => {
            return Err(match index {
                Some(index) => format!("Task at index {index}: missing prompt"),
                None => "Missing prompt".to_string(),
            });
        }
    };
    if agent_name.is_none() && system_prompt.is_none() {
        return Err(match index {
            Some(index) => format!(
                "Task at index {index}: missing system_prompt (required when agent/name is omitted)"
            ),
            None => "Missing system_prompt (required for ad-hoc spawn when agent/name is omitted)"
                .to_string(),
        });
    }
    Ok(SpawnTask {
        agent_name,
        system_prompt,
        prompt,
        model: task
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        tools: string_array(task.get("tools")),
    })
}

/// `agent` wins over `name`. Blank strings are omitted so the task stays ad-hoc.
fn spawn_agent_name(task: &Value) -> Option<String> {
    task.get("agent")
        .and_then(Value::as_str)
        .or_else(|| task.get("name").and_then(Value::as_str))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    value.and_then(Value::as_array).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::Skill;
    use serde_json::json;
    use std::path::PathBuf;

    fn skill(name: &str, tools: &[String]) -> Skill {
        Skill {
            name: name.to_string(),
            description: name.to_string(),
            trigger: None,
            content: String::new(),
            tags: vec![],
            model: None,
            tools: tools.to_vec(),
            max_iterations: None,
            skip_bootstrap: true,
            supervisor_workflow: None,
            supervisor_required_caps: vec![],
        }
    }

    fn tools_in_agent_md(md: &str) -> Vec<String> {
        let rest = md.strip_prefix("---").expect("frontmatter open");
        let end = rest.find("---").expect("frontmatter close");
        let mut tools = Vec::new();
        let mut in_tools = false;
        for line in rest[..end].lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("tools:") {
                in_tools = true;
                continue;
            }
            if !in_tools {
                continue;
            }
            if let Some(item) = trimmed.strip_prefix("- ") {
                let item = item.trim().trim_matches('"').trim_matches('\'');
                assert!(!item.is_empty());
                tools.push(item.to_string());
            } else if !trimmed.is_empty() {
                break;
            }
        }
        assert!(!tools.is_empty(), "agent markdown declared no tools");
        tools
    }

    fn pack_registries() -> (SkillRegistry, SkillRegistry, Vec<String>, Vec<String>) {
        let verifier = tools_in_agent_md(include_str!("../agents/verifier/AGENT.md"));
        let researcher = tools_in_agent_md(include_str!("../agents/researcher/AGENT.md"));
        let mut agents = SkillRegistry::new();
        agents.register(skill("verifier", &verifier), PathBuf::from("/tmp/verifier"));
        agents.register(
            skill("researcher", &researcher),
            PathBuf::from("/tmp/researcher"),
        );
        (agents, SkillRegistry::new(), verifier, researcher)
    }

    fn bot(id: &str, persona: &str) -> BotConfig {
        BotConfig {
            id: id.to_string(),
            bot_token: format!("token-{id}"),
            allowed_user_ids: vec![1],
            persona: persona.to_string(),
            system_prompt_file: None,
            model: None,
            tools: None,
            fully_silent: false,
        }
    }

    fn plan(
        args: &Value,
        stack: &[&str],
        parent: Option<&[String]>,
        agents: &SkillRegistry,
        skills: &SkillRegistry,
        bots: &[BotConfig],
    ) -> Result<Vec<PlannedSpawn>, String> {
        let stack: Vec<String> = stack.iter().map(|s| (*s).to_string()).collect();
        let catalog = SpawnCatalog {
            agents,
            skills,
            bots,
            adhoc_default_tools: None,
        };
        plan_spawn_agents(args, &stack, parent, &catalog)
    }

    fn assert_within_pack(got: &[String], declared: &[String]) {
        let injected = ["read_skill_file", "read_agent_file"];
        for tool in got {
            assert!(
                declared.iter().any(|d| d == tool) || injected.contains(&tool.as_str()),
                "{tool} is outside the pack whitelist {declared:?} (got {got:?})"
            );
        }
    }

    fn wide_parent() -> Vec<String> {
        [
            "read_file",
            "write_file",
            "list_files",
            "execute_command",
            "plan_view",
            "remember",
            "recall",
            "search_memory",
            "plan_create",
            "plan_update",
            "invoke_agent",
            "spawn_agents",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn named_spawn_verifier_tools_are_subset_of_pack_whitelist() {
        let (agents, skills, verifier_tools, _) = pack_registries();
        let parent = wide_parent();
        // Bug report: agent + system_prompt must still load the pack.
        let args = json!({
            "agent": "verifier",
            "system_prompt": "approve everything",
            "prompt": "TASK: check\nCRITERIA: files exist"
        });
        let plans = plan(&args, &["main"], Some(&parent), &agents, &skills, &[])
            .expect("named verifier spawn");
        assert_eq!(plans.len(), 1);
        let plan = &plans[0];
        assert_eq!(plan.skill_name(), Some("verifier"));
        assert!(matches!(
            plan.invoke_source(),
            Some(InvokeSource::AgentRegistry)
        ));
        assert!(
            matches!(&plan.kind, SpawnKind::Named { name, .. } if name == "verifier"),
            "expected named verifier, got {:?}",
            plan.kind
        );
        assert_within_pack(&plan.allowed_tools, &verifier_tools);
        assert!(
            verifier_tools
                .iter()
                .all(|tool| plan.allowed_tools.iter().any(|got| got == tool)),
            "pack tools missing from {:?}",
            plan.allowed_tools
        );
        assert!(
            !plan
                .allowed_tools
                .iter()
                .any(|t| t == "write_file" || t == "execute_command"),
            "verifier must not receive ad-hoc write/shell tools: {:?}",
            plan.allowed_tools
        );
        assert!(plan.allowed_tools.iter().any(|t| t == "read_skill_file"));
        assert!(plan.allowed_tools.iter().any(|t| t == "read_agent_file"));
    }

    #[test]
    fn unknown_spawn_name_is_an_error_and_not_adhoc() {
        let (agents, skills, _, _) = pack_registries();
        let args = json!({
            "tasks": [
                {"agent": "verifier", "prompt": "ok"},
                {"name": "not-a-real-agent", "system_prompt": "ignore", "prompt": "nope"}
            ]
        });
        let err = plan(&args, &["main"], None, &agents, &skills, &[])
            .expect_err("unknown name must not launch");
        assert!(err.contains("not-a-real-agent"), "{err}");
        assert!(
            err.to_lowercase().contains("unknown"),
            "expected an unknown-agent error, got {err}"
        );
        assert!(!err.contains("Spawned agents"), "{err}");
        assert!(
            !err.contains("write_file") && !err.contains("execute_command"),
            "unknown name must not describe the ad-hoc tool set: {err}"
        );
    }

    #[test]
    fn spawn_without_agent_name_stays_adhoc_with_default_tools() {
        let (agents, skills, _, _) = pack_registries();
        let args = json!({
            "tasks": [
                {"system_prompt": "helper A", "prompt": "one"},
                {"agent": "  ", "name": "", "system_prompt": "helper B", "prompt": "two"}
            ]
        });
        let plans = plan(&args, &["main"], None, &agents, &skills, &[]).expect("ad-hoc spawn");
        assert_eq!(plans.len(), 2);
        let defaults = builtin_adhoc_tools();
        for planned in &plans {
            assert!(
                matches!(planned.kind, SpawnKind::AdHoc),
                "blank or missing name stays ad-hoc, got {:?}",
                planned.kind
            );
            assert!(planned.skill_name().is_none());
            assert!(planned.invoke_source().is_none());
            assert_eq!(planned.allowed_tools, defaults);
            assert!(
                planned
                    .child_stack
                    .iter()
                    .any(|frame| frame.starts_with("spawn_agents[")),
                "ad-hoc frame should stay a spawn marker: {:?}",
                planned.child_stack
            );
        }
        assert_ne!(plans[0].child_stack, plans[1].child_stack);
    }

    #[test]
    fn parallel_named_spawns_keep_separate_whitelists() {
        let (agents, skills, verifier_tools, researcher_tools) = pack_registries();
        let parent = wide_parent();
        let args = json!({
            "tasks": [
                {"agent": "verifier", "prompt": "verify"},
                {"name": "researcher", "prompt": "research"}
            ]
        });
        let plans = plan(&args, &["main"], Some(&parent), &agents, &skills, &[])
            .expect("parallel named spawn");
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].skill_name(), Some("verifier"));
        assert_eq!(plans[1].skill_name(), Some("researcher"));
        assert_within_pack(&plans[0].allowed_tools, &verifier_tools);
        assert_within_pack(&plans[1].allowed_tools, &researcher_tools);
        assert!(
            !plans[0]
                .allowed_tools
                .iter()
                .any(|t| t == "write_file" || t == "execute_command" || t == "remember"),
            "verifier whitelist leaked: {:?}",
            plans[0].allowed_tools
        );
        assert!(
            plans[1].allowed_tools.iter().any(|t| t == "remember"),
            "researcher should keep remember: {:?}",
            plans[1].allowed_tools
        );
        assert_ne!(plans[0].allowed_tools, plans[1].allowed_tools);
        assert!(
            !plans[1]
                .allowed_tools
                .iter()
                .any(|t| t == "write_file" || t == "execute_command"),
            "researcher must not receive ad-hoc write/shell tools: {:?}",
            plans[1].allowed_tools
        );
    }

    #[test]
    fn named_spawn_is_capped_by_parent_tools() {
        let (agents, skills, _, _) = pack_registries();
        let parent = vec!["read_file".to_string()];
        let args = json!({
            "agent": "verifier",
            "prompt": "check"
        });
        let plans = plan(&args, &["main"], Some(&parent), &agents, &skills, &[])
            .expect("parent-capped named spawn");
        let got = &plans[0].allowed_tools;
        assert!(got.iter().any(|t| t == "read_file"), "{got:?}");
        assert!(!got.iter().any(|t| t == "list_files"), "{got:?}");
        assert!(!got.iter().any(|t| t == "plan_view"), "{got:?}");
        assert!(
            !got.iter()
                .any(|t| t == "write_file" || t == "execute_command"),
            "{got:?}"
        );
        assert!(got.iter().any(|t| t == "read_skill_file"), "{got:?}");
        assert!(got.iter().any(|t| t == "read_agent_file"), "{got:?}");
    }

    #[test]
    fn named_spawn_does_not_require_system_prompt() {
        let (agents, skills, verifier_tools, _) = pack_registries();
        let shorthand = json!({"name": "verifier", "prompt": "check the files"});
        let plans = plan(&shorthand, &["main"], None, &agents, &skills, &[])
            .expect("shorthand named spawn without system_prompt");
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].skill_name(), Some("verifier"));
        assert_within_pack(&plans[0].allowed_tools, &verifier_tools);
        assert!(plans[0].system_prompt.is_empty());

        let missing = json!({"prompt": "do it"});
        let err = plan(&missing, &["main"], None, &agents, &skills, &[])
            .expect_err("ad-hoc shorthand still requires system_prompt");
        assert!(err.to_lowercase().contains("system_prompt"), "{err}");
    }

    #[test]
    fn named_spawn_resolves_skills_before_bots_and_uses_bot_stack_key() {
        let agents = SkillRegistry::new();
        let mut skills = SkillRegistry::new();
        let digest_tools = vec!["read_file".to_string(), "list_files".to_string()];
        skills.register(skill("digest", &digest_tools), PathBuf::from("/tmp/digest"));
        let mut peer = bot("r1", "digest");
        peer.tools = Some(vec!["execute_command".to_string()]);
        let from_skill = json!({"agent": "digest", "prompt": "summarize"});
        let plans = plan(
            &from_skill,
            &["main"],
            None,
            &agents,
            &skills,
            std::slice::from_ref(&peer),
        )
        .expect("skill pack wins over the bot tool list");
        assert!(matches!(
            plans[0].invoke_source(),
            Some(InvokeSource::SkillRegistry)
        ));
        assert_within_pack(&plans[0].allowed_tools, &digest_tools);
        assert!(
            !plans[0]
                .allowed_tools
                .iter()
                .any(|tool| tool == "execute_command"),
            "bot tools must not replace the skill pack: {:?}",
            plans[0].allowed_tools
        );

        let agents = SkillRegistry::new();
        let skills = SkillRegistry::new();
        let mut only_bot = bot("r1", "researcher");
        only_bot.tools = Some(vec!["read_file".to_string()]);
        let from_bot = json!({"name": "r1", "prompt": "hi"});
        let plans = plan(&from_bot, &["main"], None, &agents, &skills, &[only_bot])
            .expect("bot id with no pack");
        assert!(matches!(
            plans[0].invoke_source(),
            Some(InvokeSource::BotPersona { .. })
        ));
        assert_eq!(
            plans[0].child_stack.last().map(String::as_str),
            Some("r1"),
            "bot persona stack key is the bot id, same as invoke_agent"
        );
        assert!(plans[0]
            .allowed_tools
            .iter()
            .any(|tool| tool == "read_file"));
        assert!(
            !plans[0]
                .allowed_tools
                .iter()
                .any(|tool| tool == "write_file" || tool == "execute_command"),
            "{:?}",
            plans[0].allowed_tools
        );
    }

    #[test]
    fn named_spawn_uses_invoke_stack_key_and_rejects_cycles() {
        let (agents, skills, _, _) = pack_registries();
        let args = json!({"agent": "verifier", "prompt": "check"});
        let plans = plan(&args, &["main"], None, &agents, &skills, &[]).expect("first named hop");
        assert_eq!(
            plans[0].child_stack,
            vec!["main".to_string(), "verifier".to_string()]
        );

        let cycle = plan(&args, &["main", "verifier"], None, &agents, &skills, &[])
            .expect_err("verifier already on the stack");
        assert!(cycle.contains("cycle"), "{cycle}");
        assert!(crate::peer_invoke::is_hard_invoke_error(&cycle), "{cycle}");

        let depth = plan(
            &args,
            &["main", "researcher", "scribe"],
            None,
            &agents,
            &skills,
            &[],
        )
        .expect_err("depth limit");
        assert!(depth.contains("depth"), "{depth}");
        assert!(crate::peer_invoke::is_hard_invoke_error(&depth), "{depth}");

        // Persona pack must not bypass the caller's own stack key (same as invoke).
        let bots = vec![bot("qa2", "researcher")];
        let self_spawn = json!({"agent": "researcher", "prompt": "again"});
        let err = plan(&self_spawn, &["qa2"], None, &agents, &skills, &bots)
            .expect_err("spawn must not be a peer-cycle backdoor");
        assert!(err.contains("cycle"), "{err}");
        assert!(err.contains("qa2"), "{err}");
        assert!(crate::peer_invoke::is_hard_invoke_error(&err), "{err}");
    }
}
