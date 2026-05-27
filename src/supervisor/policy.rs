use crate::config::RiskThresholdsConfig;
use crate::supervisor::task::{RiskLevel, Task, TaskType};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    AutoExecute,
    Clarify,
    RequireApproval,
    UseFallbackBackend(String),
    StopAndReport(String),
}

#[derive(Default)]
pub struct PolicyEngine {
    thresholds: RiskThresholdsConfig,
}

impl PolicyEngine {
    pub fn with_thresholds(thresholds: RiskThresholdsConfig) -> Self {
        Self { thresholds }
    }

    pub fn thresholds(&self) -> &RiskThresholdsConfig {
        &self.thresholds
    }

    pub fn decide(&self, t: &Task) -> PolicyDecision {
        if t.risk_level == RiskLevel::High {
            return PolicyDecision::RequireApproval;
        }
        if t.risk_level == RiskLevel::Medium && self.thresholds.require_approval_for_medium {
            return PolicyDecision::RequireApproval;
        }
        if t.risk_level == RiskLevel::Low && self.thresholds.require_approval_for_low {
            return PolicyDecision::RequireApproval;
        }
        if t.task_type == TaskType::Unknown && t.risk_level == RiskLevel::Low {
            return PolicyDecision::Clarify;
        }
        // `auto_execute_only_low`: when enabled, Medium-risk tasks need
        // explicit approval even though they aren't High.
        if t.risk_level == RiskLevel::Medium && self.thresholds.auto_execute_only_low {
            return PolicyDecision::RequireApproval;
        }
        PolicyDecision::AutoExecute
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_risk_well_scoped_auto_executes() {
        use crate::supervisor::task::*;
        let mut t = Task::new("ok", "ok");
        t.task_type = TaskType::GeneralAssistant;
        t.risk_level = RiskLevel::Low;
        let d = PolicyEngine::default().decide(&t);
        assert_eq!(d, PolicyDecision::AutoExecute);
    }

    #[test]
    fn high_risk_requires_approval() {
        use crate::supervisor::task::*;
        let mut t = Task::new("rm -rf /", "delete prod");
        t.risk_level = RiskLevel::High;
        let d = PolicyEngine::default().decide(&t);
        assert_eq!(d, PolicyDecision::RequireApproval);
    }

    #[test]
    fn ambiguous_task_triggers_clarification() {
        use crate::supervisor::task::*;
        let mut t = Task::new("do the thing", "do the thing");
        t.task_type = TaskType::Unknown;
        t.risk_level = RiskLevel::Low;
        let d = PolicyEngine::default().decide(&t);
        assert_eq!(d, PolicyDecision::Clarify);
    }

    #[test]
    fn risk_thresholds_can_be_tightened_via_config() {
        use crate::supervisor::task::*;
        let mut t = Task::new("x", "x");
        t.task_type = TaskType::OpsAutomation;
        t.risk_level = RiskLevel::Medium;
        let policy = PolicyEngine::with_thresholds(crate::config::RiskThresholdsConfig {
            require_approval_for_medium: true,
            ..Default::default()
        });
        assert_eq!(policy.decide(&t), PolicyDecision::RequireApproval);
    }

    #[test]
    fn default_thresholds_preserve_m1_behavior() {
        use crate::supervisor::task::*;
        let mut t = Task::new("refactor x", "refactor x");
        t.task_type = TaskType::Refactor;
        t.risk_level = RiskLevel::Medium;
        assert_eq!(
            PolicyEngine::default().decide(&t),
            PolicyDecision::AutoExecute
        );
    }
}
