use crate::supervisor::task::{ExecutionMode, RiskLevel, Task, TaskType};

pub struct ClassificationOutcome {
    pub task_type: TaskType,
    pub risk_level: RiskLevel,
    pub execution_mode: ExecutionMode,
    pub required_capabilities: Vec<String>,
    pub confidence: f32,
}

pub trait Classifier {
    fn classify(&self, request: &str) -> ClassificationOutcome;
}

pub struct HeuristicClassifier;

impl Classifier for HeuristicClassifier {
    fn classify(&self, request: &str) -> ClassificationOutcome {
        let lower = request.to_lowercase();
        let (task_type, risk, caps) = if lower.starts_with("rename ")
            || lower.contains("refactor")
            || lower.contains("rewrite")
        {
            (
                TaskType::Refactor,
                RiskLevel::Medium,
                vec!["coding".into(), "shell".into()],
            )
        } else if lower.starts_with("fix ") || lower.contains("bug") {
            (TaskType::BugFix, RiskLevel::Medium, vec!["coding".into()])
        } else if lower.starts_with("research") || lower.starts_with("compare") {
            (
                TaskType::Research,
                RiskLevel::Low,
                vec!["research".into(), "reasoning".into()],
            )
        } else if lower.starts_with("summarize") || lower.starts_with("answer ") {
            (
                TaskType::GeneralAssistant,
                RiskLevel::Low,
                vec!["reasoning".into()],
            )
        } else if lower.starts_with("write ") || lower.contains("draft ") {
            (
                TaskType::Writing,
                RiskLevel::Low,
                vec!["document".into(), "reasoning".into()],
            )
        } else if lower.starts_with("run ") || lower.contains("script") || lower.contains("shell") {
            (
                TaskType::OpsAutomation,
                RiskLevel::Medium,
                vec!["shell".into()],
            )
        } else {
            (TaskType::Unknown, RiskLevel::Low, vec!["reasoning".into()])
        };

        let exec = match (&task_type, &risk) {
            (_, RiskLevel::High) => ExecutionMode::Rigorous,
            (TaskType::CodeChange, _) | (TaskType::Refactor, _) | (TaskType::BugFix, _) => {
                ExecutionMode::Rigorous
            }
            (TaskType::GeneralAssistant, _) => ExecutionMode::Fast,
            _ => ExecutionMode::Standard,
        };
        ClassificationOutcome {
            task_type,
            risk_level: risk,
            execution_mode: exec,
            required_capabilities: caps,
            confidence: 0.6,
        }
    }
}

impl HeuristicClassifier {
    pub fn classify_as_task(&self, request: &str) -> Task {
        let mut t = Task::new(request.lines().next().unwrap_or(request), request);
        let o = <Self as Classifier>::classify(self, request);
        t.task_type = o.task_type;
        t.risk_level = o.risk_level;
        t.execution_mode = o.execution_mode;
        t.required_capabilities = o.required_capabilities;
        t
    }
}

pub struct LlmBackedClassifier {
    #[allow(dead_code)]
    inner_llm: Option<crate::llm::LlmClient>,
    fallback: HeuristicClassifier,
}

impl LlmBackedClassifier {
    pub fn new(llm: crate::llm::LlmClient) -> Self {
        Self {
            inner_llm: Some(llm),
            fallback: HeuristicClassifier,
        }
    }
    pub fn heuristic_only() -> Self {
        Self {
            inner_llm: None,
            fallback: HeuristicClassifier,
        }
    }
}

impl Classifier for LlmBackedClassifier {
    fn classify(&self, request: &str) -> ClassificationOutcome {
        // M1: only the heuristic path is wired. The async LLM call is added in M3
        // because it requires the agent loop. For now we always use the fallback.
        <HeuristicClassifier as Classifier>::classify(&self.fallback, request)
    }
}

/// Wraps a base [`Classifier`] and consults a [`SkillRegistry`] to override the
/// required-capabilities list when the request mentions a known supervisor skill pack.
pub struct SkillAwareClassifier<C: Classifier> {
    inner: C,
    skills: crate::skills::SkillRegistry,
}

impl<C: Classifier> SkillAwareClassifier<C> {
    pub fn new(inner: C, skills: crate::skills::SkillRegistry) -> Self {
        Self { inner, skills }
    }

    pub fn classify(&self, request: &str) -> Task {
        let mut base = HeuristicClassifier.classify_as_task(request);
        let outcome = self.inner.classify(request);
        base.task_type = outcome.task_type;
        base.risk_level = outcome.risk_level;
        base.execution_mode = outcome.execution_mode;
        base.required_capabilities = outcome.required_capabilities;

        // Match request against skill packs by simple keyword: name without "sup-" prefix.
        let lower = request.to_lowercase();
        for skill in self.skills.list() {
            let key = skill.name.strip_prefix("sup-").unwrap_or(&skill.name);
            if lower.contains(key) && skill.supervisor_workflow.is_some() {
                base.required_capabilities = skill.supervisor_required_caps.clone();
                break;
            }
        }
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_classifier_falls_back_to_heuristic_when_disabled() {
        let c = LlmBackedClassifier::heuristic_only();
        let o = c.classify("summarize the readme");
        assert_eq!(
            o.task_type,
            crate::supervisor::task::TaskType::GeneralAssistant
        );
    }

    #[test]
    fn heuristic_classifies_obvious_cases() {
        use crate::supervisor::task::{RiskLevel, TaskType};
        let c = HeuristicClassifier;
        let t = c.classify_as_task("rename foo() to bar() in src/lib.rs");
        assert_eq!(t.task_type, TaskType::Refactor);
        assert!(matches!(t.risk_level, RiskLevel::Medium | RiskLevel::High));

        let t = c.classify_as_task("summarize the file ./README.md");
        assert_eq!(t.task_type, TaskType::GeneralAssistant);
        assert_eq!(t.risk_level, RiskLevel::Low);

        let t = c.classify_as_task("research best Rust async runtime 2026");
        assert_eq!(t.task_type, TaskType::Research);
    }

    #[tokio::test]
    async fn skill_hint_overrides_default_workflow() {
        let mut registry = crate::skills::SkillRegistry::new();
        registry.register(
            crate::skills::Skill {
                name: "sup-research".into(),
                description: "research".into(),
                content: "".into(),
                tags: vec![],
                model: None,
                tools: vec![],
                max_iterations: None,
                supervisor_workflow: Some("research".into()),
                supervisor_required_caps: vec!["research".into()],
            },
            crate::skills::SkillSource::Instance,
            std::path::PathBuf::from("/tmp/test"),
        );
        let c = SkillAwareClassifier::new(HeuristicClassifier, registry);
        // Request must contain the skill's keyword ("research", from "sup-research") for the
        // hint to fire; the heuristic still classifies it as GeneralAssistant on the
        // "answer " starts_with path, so the only way capabilities change is via the skill hint.
        let t = c.classify("answer this question about research: foo");
        // Heuristic alone returns GeneralAssistant (caps=["reasoning"]),
        // but the skill hint elevates required_capabilities to ["research"].
        assert_eq!(
            t.task_type,
            crate::supervisor::task::TaskType::GeneralAssistant
        );
        assert_eq!(t.required_capabilities, vec!["research"]);
    }
}
