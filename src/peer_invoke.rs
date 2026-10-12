//! Peer invoke guards and attribution (design §7.5 / PO §8b).
//!
//! Nested `invoke_agent` across bot personas stays in-process (shared sandbox,
//! caller's Telegram chat). No Telegram bot↔bot messaging, no message bus.

use crate::config::BotConfig;

/// Maximum peer-invoke frames beyond the root caller.
///
/// Chain: main → peer → peer's subagent, then hard stop.
pub const MAX_PEER_DEPTH: usize = 2;

/// Where an `invoke_agent` name resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvokeSource {
    /// `agents/<name>/AGENT.md` registry entry.
    AgentRegistry,
    /// Subagent skill (`skills/` with model/tools).
    SkillRegistry,
    /// `[[bots]]` entry matched by `id` or `persona`.
    BotPersona { bot_id: String, persona: String },
}

/// Reject cycles and depth overflow before pushing `target` onto `stack`.
///
/// `stack` is seeded with the root caller **bot_id**. Callers must pass a
/// canonical `target` from [`stack_key_for_invoke`] then
/// [`canonicalize_self_stack_key`] (bot personas → `bot_id`; agents/skills keep
/// their registry name unless that name is the caller's persona / bot_id).
/// Peer depth is `stack.len() - 1`.
pub fn guard_peer_invoke(stack: &[String], target: &str) -> Result<(), String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("Peer invoke target is empty".to_string());
    }
    if stack.iter().any(|id| id == target) {
        return Err(format!(
            "Peer invoke cycle detected: '{target}' is already on the invoke stack [{}]",
            stack.join(" → ")
        ));
    }
    let peer_depth = stack.len().saturating_sub(1);
    if peer_depth >= MAX_PEER_DEPTH {
        return Err(format!(
            "Peer invoke depth limit exceeded (max_peer_depth={MAX_PEER_DEPTH}): stack [{}] cannot invoke '{target}'",
            stack.join(" → ")
        ));
    }
    Ok(())
}

/// Prepend the user-visible peer attribution marker.
pub fn format_via_attribution(persona: &str, body: &str) -> String {
    let persona = persona.trim();
    if persona.is_empty() {
        return body.to_string();
    }
    if body.is_empty() {
        return format!("via {persona}:");
    }
    format!("via {persona}:\n{body}")
}

/// Resolve `name` in order: agents registry → skills registry → `[[bots]]` id/persona.
pub fn resolve_invoke_source(
    name: &str,
    in_agents: bool,
    in_skills: bool,
    bots: &[BotConfig],
) -> Option<InvokeSource> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if in_agents {
        return Some(InvokeSource::AgentRegistry);
    }
    if in_skills {
        return Some(InvokeSource::SkillRegistry);
    }
    find_bot_persona(bots, name)
        .map(|(bot_id, persona)| InvokeSource::BotPersona { bot_id, persona })
}

/// Match a bot by `id` first, then by `persona` (trim-aware).
pub fn find_bot_persona(bots: &[BotConfig], name: &str) -> Option<(String, String)> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if let Some(bot) = bots.iter().find(|b| b.id.trim() == name) {
        return Some((bot.id.trim().to_string(), bot.persona.trim().to_string()));
    }
    bots.iter()
        .find(|b| b.persona.trim() == name)
        .map(|b| (b.id.trim().to_string(), b.persona.trim().to_string()))
}

/// Canonical invoke-stack key after resolve (agents → skills → bots).
///
/// - [`InvokeSource::BotPersona`] → prefer stable `bot_id` (`persona` is an alias)
/// - Agent / skill registry (or unknown) → trimmed invoke name
pub fn stack_key_for_invoke(name: &str, source: &Option<InvokeSource>) -> String {
    match source {
        Some(InvokeSource::BotPersona { bot_id, .. }) => bot_id.clone(),
        _ => name.trim().to_string(),
    }
}

/// Where tool-call status and results should be delivered.
///
/// A peer invoke of a `[[bots]]` persona routes to **that** bot. Other
/// subagents stay on the caller bot so a secondary bot is not funneled
/// through the shim/main sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDelivery {
    pub bot_id: String,
    pub user_id: String,
    pub chat_id: String,
    /// Persist the peer turn (tool calls included) on this bot's conversation.
    pub record_on_bot: bool,
}

pub fn tool_delivery_for_invoke(
    caller_bot_id: &str,
    source: Option<&InvokeSource>,
    user_id: &str,
    chat_id: &str,
) -> ToolDelivery {
    match source {
        Some(InvokeSource::BotPersona { bot_id, .. }) => ToolDelivery {
            bot_id: bot_id.clone(),
            user_id: user_id.to_string(),
            chat_id: chat_id.to_string(),
            record_on_bot: true,
        },
        _ => ToolDelivery {
            bot_id: crate::platform::normalize_bot_id(caller_bot_id).to_string(),
            user_id: user_id.to_string(),
            chat_id: chat_id.to_string(),
            record_on_bot: false,
        },
    }
}

/// Rewrite the invoke stack key when the target is the **current** bot on the
/// stack — by `bot_id` or that bot's `persona` — so AgentRegistry / SkillRegistry
/// packs named like the caller's persona hard-reject as self (same cycle path).
///
/// Cross-persona peer invoke is unchanged: only frames already on `stack` count.
/// Example: bot `qa2` (persona=`researcher`) with stack `["qa2"]` invoking
/// agent pack `"researcher"` → key `"qa2"` → [`guard_peer_invoke`] cycle error.
pub fn canonicalize_self_stack_key(
    stack: &[String],
    invoke_name: &str,
    preliminary_key: &str,
    bots: &[BotConfig],
) -> String {
    let name = invoke_name.trim();
    let key = preliminary_key.trim();
    if name.is_empty() && key.is_empty() {
        return preliminary_key.to_string();
    }

    for frame in stack {
        let frame = frame.as_str();
        if key == frame || name == frame {
            return frame.to_string();
        }
        if let Some(bot) = bots.iter().find(|b| b.id.trim() == frame) {
            let persona = bot.persona.trim();
            if !persona.is_empty() && (name == persona || key == persona) {
                return bot.id.trim().to_string();
            }
        }
    }
    key.to_string()
}

/// Look up the bot config for a peer target (id or persona).
pub fn bot_config_for_peer<'a>(bots: &'a [BotConfig], name: &str) -> Option<&'a BotConfig> {
    let name = name.trim();
    bots.iter()
        .find(|b| b.id.trim() == name)
        .or_else(|| bots.iter().find(|b| b.persona.trim() == name))
}

/// Resolve main-loop model + tools for a bot turn (design §7.6).
///
/// Precedence:
/// 1. Explicit `bots[].model` / `bots[].tools` if set
/// 2. Else persona `agents/<persona>/AGENT.md` frontmatter (`persona_model` /
///    `persona_tools` from the agents registry)
/// 3. Else `None` — caller uses install defaults (full tool registry +
///    `[openrouter].model` / current model)
///
/// Empty `persona_tools` means "no AGENT.md whitelist" (not an empty allowlist).
pub fn resolve_bot_loop_overrides(
    bot: &BotConfig,
    persona_model: Option<&str>,
    persona_tools: &[String],
) -> (Option<String>, Option<Vec<String>>) {
    let model = bot
        .model
        .clone()
        .or_else(|| persona_model.map(str::to_string));
    let tools = bot.tools.clone().or_else(|| {
        if persona_tools.is_empty() {
            None
        } else {
            Some(persona_tools.to_vec())
        }
    });
    (model, tools)
}

/// True when an `invoke_agent` / peer-guard failure must abort the turn
/// (clear Telegram Working + surface an error reply) instead of continuing
/// the agentic loop with a soft tool result.
pub fn is_hard_invoke_error(msg: &str) -> bool {
    let m = msg.trim();
    m.starts_with("Peer invoke cycle")
        || m.starts_with("Peer invoke depth")
        || m.starts_with("Peer invoke target")
        || m.starts_with("Peer invoke rejected")
}

/// `bots[].id` that differs from `bots[].persona` is a Telegram bot identity,
/// not an agents-pack name. Pack lookup requires the persona map (or id==persona).
pub fn bot_id_is_unmapped_pack_name(bots: &[BotConfig], name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    bots.iter().any(|b| {
        let id = b.id.trim();
        let persona = b.persona.trim();
        id == name && id != persona
    })
}

/// Push a canonical `target` onto a cloned stack (caller must have already guarded).
pub fn push_invoke_stack(stack: &[String], target: &str) -> Vec<String> {
    let mut next = stack.to_vec();
    next.push(target.trim().to_string());
    next
}

/// Child invoke stack for the `index`-th ad-hoc task of a `spawn_agents` call.
///
/// Pushes a depth-unique `spawn_agents[<depth>.<index>]` frame so nested
/// spawns count toward [`MAX_PEER_DEPTH`] through [`guard_peer_invoke`]
/// without tripping its cycle check. Returns the guard's error string when the
/// stack is already at the depth limit.
pub fn spawn_agents_child_stack(stack: &[String], index: usize) -> Result<Vec<String>, String> {
    let marker = format!("spawn_agents[{}.{index}]", stack.len());
    guard_peer_invoke(stack, &marker)?;
    Ok(push_invoke_stack(stack, &marker))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn depth_limit_allows_two_peer_hops_then_rejects() {
        // root only — peer_depth 0
        assert!(guard_peer_invoke(&["main".into()], "researcher").is_ok());
        // after one peer — peer_depth 1
        assert!(guard_peer_invoke(&["main".into(), "researcher".into()], "verifier").is_ok());
        // after two peers — peer_depth 2 ≥ MAX → reject
        let err = guard_peer_invoke(
            &["main".into(), "researcher".into(), "verifier".into()],
            "other",
        )
        .unwrap_err();
        assert!(
            err.contains("depth limit") && err.contains("max_peer_depth=2"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn cycle_reject_when_target_already_on_stack() {
        let err = guard_peer_invoke(&["main".into(), "researcher".into()], "main").unwrap_err();
        assert!(
            err.contains("cycle") && err.contains("'main'"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn cycle_reject_self_invoke() {
        let err = guard_peer_invoke(&["researcher".into()], "researcher").unwrap_err();
        assert!(err.contains("cycle"), "unexpected: {err}");
    }

    #[test]
    fn attribution_prepends_via_marker() {
        assert_eq!(
            format_via_attribution("researcher", "findings here"),
            "via researcher:\nfindings here"
        );
        assert_eq!(format_via_attribution("verifier", ""), "via verifier:");
    }

    #[test]
    fn persona_resolution_prefers_agents_then_skills_then_bots() {
        let bots = vec![bot("researcher", "researcher")];
        assert_eq!(
            resolve_invoke_source("verifier", true, false, &bots),
            Some(InvokeSource::AgentRegistry)
        );
        assert_eq!(
            resolve_invoke_source("thread-writer", false, true, &bots),
            Some(InvokeSource::SkillRegistry)
        );
        assert_eq!(
            resolve_invoke_source("researcher", false, false, &bots),
            Some(InvokeSource::BotPersona {
                bot_id: "researcher".into(),
                persona: "researcher".into(),
            })
        );
        assert!(resolve_invoke_source("ghost", false, false, &bots).is_none());
    }

    #[test]
    fn persona_resolution_matches_bot_id_or_persona() {
        let bots = vec![bot("r1", "researcher")];
        assert_eq!(
            resolve_invoke_source("r1", false, false, &bots),
            Some(InvokeSource::BotPersona {
                bot_id: "r1".into(),
                persona: "researcher".into(),
            })
        );
        assert_eq!(
            resolve_invoke_source("researcher", false, false, &bots),
            Some(InvokeSource::BotPersona {
                bot_id: "r1".into(),
                persona: "researcher".into(),
            })
        );
    }

    #[test]
    fn push_invoke_stack_appends_target() {
        assert_eq!(
            push_invoke_stack(&["main".into()], "researcher"),
            vec!["main".to_string(), "researcher".to_string()]
        );
    }

    #[test]
    fn stack_key_bot_persona_prefers_bot_id() {
        let bots = vec![bot("r1", "researcher")];
        let by_persona = resolve_invoke_source("researcher", false, false, &bots);
        let by_id = resolve_invoke_source("r1", false, false, &bots);
        assert_eq!(stack_key_for_invoke("researcher", &by_persona), "r1");
        assert_eq!(stack_key_for_invoke("r1", &by_id), "r1");
        // Agent registry wins over bots — keep registry name.
        let agent_src = resolve_invoke_source("researcher", true, false, &bots);
        assert_eq!(stack_key_for_invoke("researcher", &agent_src), "researcher");
    }

    #[test]
    fn cycle_reject_persona_alias_when_stack_has_bot_id() {
        // TL HOLD: stack seeded with bot_id=r1; invoke by persona=researcher
        // must canonicalize to r1 and reject as self/cycle.
        let bots = vec![bot("r1", "researcher")];
        let stack = vec!["r1".to_string()];
        let source = resolve_invoke_source("researcher", false, false, &bots);
        let key = stack_key_for_invoke("researcher", &source);
        assert_eq!(key, "r1", "persona alias must resolve to bot_id");
        let err = guard_peer_invoke(&stack, &key).unwrap_err();
        assert!(
            err.contains("cycle") && err.contains("'r1'"),
            "unexpected: {err}"
        );
        // Symmetric: stack has bot_id, invoke by id also rejects.
        let source_id = resolve_invoke_source("r1", false, false, &bots);
        let key_id = stack_key_for_invoke("r1", &source_id);
        let err_id = guard_peer_invoke(&stack, &key_id).unwrap_err();
        assert!(err_id.contains("cycle"), "unexpected: {err_id}");
    }

    #[test]
    fn resolve_bot_loop_overrides_bots_fields_win() {
        let mut b = bot("r1", "researcher");
        b.model = Some("bots/model".into());
        b.tools = Some(vec!["read_file".into()]);
        let (m, t) = resolve_bot_loop_overrides(&b, Some("persona/model"), &["list_files".into()]);
        assert_eq!(m.as_deref(), Some("bots/model"));
        assert_eq!(t.as_deref(), Some(["read_file".to_string()].as_slice()));
    }

    #[test]
    fn resolve_bot_loop_overrides_falls_back_to_persona_agent_md() {
        let b = bot("r1", "researcher");
        let persona_tools = vec!["read_file".into(), "list_files".into()];
        let (m, t) = resolve_bot_loop_overrides(&b, Some("persona/model"), &persona_tools);
        assert_eq!(m.as_deref(), Some("persona/model"));
        assert_eq!(t.as_ref().map(|v| v.len()), Some(2));
        assert!(t.unwrap().contains(&"list_files".to_string()));
    }

    #[test]
    fn resolve_bot_loop_overrides_empty_persona_tools_means_install_default() {
        let b = bot("main", "main");
        let (m, t) = resolve_bot_loop_overrides(&b, None, &[]);
        assert!(m.is_none());
        assert!(t.is_none());
    }

    #[test]
    fn unmapped_bot_id_resolves_as_persona_and_hard_errors_on_self() {
        let bots = vec![bot("qa2", "researcher"), bot("qa", "main")];
        assert!(bot_id_is_unmapped_pack_name(&bots, "qa2"));
        // Callers pass in_agents=false / in_skills=false for unmapped bot_id.
        let source = resolve_invoke_source("qa2", false, false, &bots);
        assert_eq!(
            source,
            Some(InvokeSource::BotPersona {
                bot_id: "qa2".into(),
                persona: "researcher".into(),
            })
        );
        let key = stack_key_for_invoke("qa2", &source);
        assert_eq!(key, "qa2");
        let err = guard_peer_invoke(&["qa2".into()], &key).unwrap_err();
        assert!(is_hard_invoke_error(&err), "unexpected: {err}");
        // Peer bot_id from another root is fine.
        assert!(guard_peer_invoke(&["qa".into()], &key).is_ok());
    }

    #[test]
    fn tool_delivery_peer_bot_is_not_caller_main() {
        let source = InvokeSource::BotPersona {
            bot_id: "researcher".into(),
            persona: "researcher".into(),
        };
        let d = tool_delivery_for_invoke("main", Some(&source), "42", "42");
        assert_eq!(d.bot_id, "researcher");
        assert!(d.record_on_bot);
        assert_eq!(d.user_id, "42");
        assert_eq!(d.chat_id, "42");
    }

    #[test]
    fn tool_delivery_non_bot_stays_on_caller_not_forced_main() {
        let d = tool_delivery_for_invoke("researcher", None, "7", "7");
        assert_eq!(d.bot_id, "researcher");
        assert!(!d.record_on_bot);
    }

    #[test]
    fn hard_invoke_error_detects_cycle_and_depth() {
        assert!(is_hard_invoke_error(
            "Peer invoke cycle detected: 'qa2' is already on the invoke stack [qa2]"
        ));
        assert!(is_hard_invoke_error(
            "Peer invoke depth limit exceeded (max_peer_depth=2): stack [a → b → c] cannot invoke 'x'"
        ));
        assert!(is_hard_invoke_error("Peer invoke target is empty"));
        assert!(!is_hard_invoke_error("via researcher:\nfindings"));
        assert!(!is_hard_invoke_error("Missing prompt"));
    }

    #[test]
    fn bot_id_distinct_from_persona_is_unmapped_pack_name() {
        let bots = vec![bot("qa2", "researcher"), bot("main", "main")];
        assert!(bot_id_is_unmapped_pack_name(&bots, "qa2"));
        // id == persona → may share agents/<id> pack name
        assert!(!bot_id_is_unmapped_pack_name(&bots, "main"));
        assert!(!bot_id_is_unmapped_pack_name(&bots, "researcher"));
        assert!(!bot_id_is_unmapped_pack_name(&bots, "ghost"));
    }

    #[test]
    fn agent_registry_persona_self_canonicalizes_to_caller_bot_id() {
        // TL HOLD (Product Q1): qa2 persona=researcher → invoke_agent("researcher")
        // via AgentRegistry must canonicalize to bot_id and hard-reject as self.
        let bots = vec![bot("qa2", "researcher"), bot("qa", "main")];
        let stack = vec!["qa2".to_string()];
        let source = resolve_invoke_source("researcher", true, false, &bots);
        assert_eq!(source, Some(InvokeSource::AgentRegistry));
        let prelim = stack_key_for_invoke("researcher", &source);
        assert_eq!(prelim, "researcher", "preliminary key stays registry name");
        let key = canonicalize_self_stack_key(&stack, "researcher", &prelim, &bots);
        assert_eq!(
            key, "qa2",
            "persona pack must canonicalize to stacked bot_id"
        );
        let err = guard_peer_invoke(&stack, &key).unwrap_err();
        assert!(
            err.contains("cycle") && err.contains("'qa2'"),
            "unexpected: {err}"
        );
        assert!(
            is_hard_invoke_error(&err),
            "must hard-abort like cycle: {err}"
        );

        // Cross-persona peer invoke still OK: qa → researcher agent pack.
        let stack_qa = vec!["qa".to_string()];
        let key_peer = canonicalize_self_stack_key(&stack_qa, "researcher", &prelim, &bots);
        assert_eq!(key_peer, "researcher");
        assert!(guard_peer_invoke(&stack_qa, &key_peer).is_ok());
    }

    #[test]
    fn spawn_agents_nesting_is_bounded_by_max_peer_depth() {
        let root = vec!["main".to_string()];
        // First level (main → spawn) is allowed.
        let l1 = spawn_agents_child_stack(&root, 0).unwrap();
        assert_eq!(l1.len(), 2);
        // Sibling tasks get distinct frames.
        assert_ne!(l1, spawn_agents_child_stack(&root, 1).unwrap());
        // Second level is allowed (no false cycle on the repeated tool name).
        let l2 = spawn_agents_child_stack(&l1, 0).unwrap();
        assert_eq!(l2.len(), MAX_PEER_DEPTH + 1);
        // Beyond MAX_PEER_DEPTH: rejected with the shared depth message.
        let err = spawn_agents_child_stack(&l2, 0).unwrap_err();
        assert!(err.contains("depth limit exceeded"), "{err}");
        assert!(err.contains(&format!("max_peer_depth={MAX_PEER_DEPTH}")));
        // Mixed with invoke_agent frames the same budget applies.
        let via_agent = push_invoke_stack(&root, "researcher");
        let mixed = spawn_agents_child_stack(&via_agent, 0).unwrap();
        assert!(spawn_agents_child_stack(&mixed, 0).is_err());
        assert!(guard_peer_invoke(&mixed, "verifier").is_err());
    }
}
