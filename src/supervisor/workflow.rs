use crate::supervisor::task::{ExecutionMode, Task, TaskStatus};

pub struct WorkflowTemplate {
    mode: ExecutionMode,
}

impl WorkflowTemplate {
    pub fn for_task(t: &Task) -> Self {
        Self {
            mode: t.execution_mode.clone(),
        }
    }

    pub fn stages(&self) -> Vec<TaskStatus> {
        use TaskStatus::*;
        match self.mode {
            ExecutionMode::Fast => vec![Intake, Classify, Execute, Verify, Report],
            ExecutionMode::Standard => vec![
                Intake, Classify, Route, Clarify, Plan, Execute, Verify, Report, Archive,
            ],
            ExecutionMode::Rigorous => vec![
                Intake,
                Classify,
                Route,
                Clarify,
                Plan,
                PrepareWorkspace,
                Execute,
                Review,
                Verify,
                Report,
                Archive,
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_mode_skips_clarify_and_plan() {
        use crate::supervisor::task::*;
        let mut t = Task::new("x", "summarize");
        t.execution_mode = ExecutionMode::Fast;
        let stages = WorkflowTemplate::for_task(&t).stages();
        assert_eq!(
            stages,
            vec![
                TaskStatus::Intake,
                TaskStatus::Classify,
                TaskStatus::Execute,
                TaskStatus::Verify,
                TaskStatus::Report,
            ]
        );
    }

    #[test]
    fn rigorous_includes_review_and_archive() {
        use crate::supervisor::task::*;
        let mut t = Task::new("x", "x");
        t.execution_mode = ExecutionMode::Rigorous;
        let stages = WorkflowTemplate::for_task(&t).stages();
        assert!(stages.contains(&TaskStatus::Review));
        assert!(stages.contains(&TaskStatus::Archive));
    }
}
