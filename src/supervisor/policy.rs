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
pub struct PolicyEngine;

impl PolicyEngine {
    pub fn decide(&self, t: &Task) -> PolicyDecision {
        if t.risk_level == RiskLevel::High {
            return PolicyDecision::RequireApproval;
        }
        if t.task_type == TaskType::Unknown && t.risk_level == RiskLevel::Low {
            return PolicyDecision::Clarify;
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
        let d = PolicyEngine.decide(&t);
        assert_eq!(d, PolicyDecision::AutoExecute);
    }

    #[test]
    fn high_risk_requires_approval() {
        use crate::supervisor::task::*;
        let mut t = Task::new("rm -rf /", "delete prod");
        t.risk_level = RiskLevel::High;
        let d = PolicyEngine.decide(&t);
        assert_eq!(d, PolicyDecision::RequireApproval);
    }

    #[test]
    fn ambiguous_task_triggers_clarification() {
        use crate::supervisor::task::*;
        let mut t = Task::new("do the thing", "do the thing");
        t.task_type = TaskType::Unknown;
        t.risk_level = RiskLevel::Low;
        let d = PolicyEngine.decide(&t);
        assert_eq!(d, PolicyDecision::Clarify);
    }
}
