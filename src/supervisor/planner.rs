use crate::supervisor::job::{Job, JobType};
use crate::supervisor::task::{ExecutionMode, Task};

pub struct Plan {
    pub jobs: Vec<Job>,
}

#[derive(Default)]
pub struct Planner;

impl Planner {
    pub fn new() -> Self {
        Self
    }

    pub fn plan(&self, t: &Task) -> Plan {
        let mut jobs = Vec::new();
        let primary_backend = t
            .required_capabilities
            .first()
            .map(String::as_str)
            .unwrap_or("reasoning")
            .to_string();
        if matches!(t.execution_mode, ExecutionMode::Rigorous) {
            jobs.push(Job::new(
                &t.id,
                JobType::PlannerJob,
                "reasoning",
                &format!("Plan steps for: {}", t.user_request),
            ));
        }
        let mut exec = Job::new(
            &t.id,
            JobType::ExecutorJob,
            &primary_backend,
            &t.user_request,
        );
        exec.prompt = Some(t.user_request.clone());
        jobs.push(exec);
        if matches!(t.execution_mode, ExecutionMode::Rigorous) {
            jobs.push(Job::new(
                &t.id,
                JobType::ReviewerJob,
                "reasoning",
                &format!("Review the executor result for: {}", t.title),
            ));
        }
        Plan { jobs }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_emits_single_executor_job_for_simple_task() {
        use crate::supervisor::task::*;
        let mut t = Task::new("ok", "summarize the readme");
        t.task_type = TaskType::GeneralAssistant;
        t.required_capabilities = vec!["reasoning".into()];
        let plan = Planner::new().plan(&t);
        assert_eq!(plan.jobs.len(), 1);
        assert_eq!(
            plan.jobs[0].job_type,
            crate::supervisor::job::JobType::ExecutorJob
        );
    }

    #[test]
    fn planner_emits_planner_then_executor_for_rigorous_code_task() {
        use crate::supervisor::task::*;
        let mut t = Task::new("refactor", "refactor module foo");
        t.task_type = TaskType::Refactor;
        t.execution_mode = ExecutionMode::Rigorous;
        t.required_capabilities = vec!["coding".into()];
        let plan = Planner::new().plan(&t);
        assert_eq!(plan.jobs.len(), 3, "planner + executor + reviewer");
        assert_eq!(
            plan.jobs[0].job_type,
            crate::supervisor::job::JobType::PlannerJob
        );
        assert_eq!(
            plan.jobs[1].job_type,
            crate::supervisor::job::JobType::ExecutorJob
        );
        assert_eq!(
            plan.jobs[2].job_type,
            crate::supervisor::job::JobType::ReviewerJob
        );
    }
}
