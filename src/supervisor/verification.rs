use crate::supervisor::job::{Job, JobStatus};

pub enum VerificationOutcome {
    Passed,
    Failed(String),
}

pub struct VerificationEngine;

impl VerificationEngine {
    pub fn verify(&self, jobs: &[Job]) -> VerificationOutcome {
        for j in jobs {
            if !matches!(j.status, JobStatus::Succeeded) {
                return VerificationOutcome::Failed(format!("job {} not succeeded", j.id));
            }
            let ev_count = j.result.as_ref().map(|r| r.evidence.len()).unwrap_or(0);
            if ev_count == 0 {
                return VerificationOutcome::Failed(format!("job {} produced no evidence", j.id));
            }
        }
        VerificationOutcome::Passed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done_job(
        status: crate::supervisor::job::JobStatus,
        ev: Vec<crate::supervisor::job::Evidence>,
    ) -> crate::supervisor::job::Job {
        let mut j = crate::supervisor::job::Job::new(
            "t",
            crate::supervisor::job::JobType::ExecutorJob,
            "reasoning",
            "g",
        );
        j.status = status.clone();
        j.result = Some(crate::supervisor::job::JobOutput {
            status,
            summary: String::new(),
            evidence: ev,
            errors: vec![],
            changed_files: vec![],
            next_step: None,
        });
        j
    }

    #[test]
    fn verifies_when_all_jobs_succeeded_with_evidence() {
        use crate::supervisor::job::*;
        let jobs = vec![done_job(JobStatus::Succeeded, vec![Evidence::ExitCode(0)])];
        assert!(matches!(
            VerificationEngine.verify(&jobs),
            VerificationOutcome::Passed
        ));
    }

    #[test]
    fn fails_when_any_job_lacks_evidence() {
        use crate::supervisor::job::*;
        let jobs = vec![done_job(JobStatus::Succeeded, vec![])];
        assert!(matches!(
            VerificationEngine.verify(&jobs),
            VerificationOutcome::Failed(_)
        ));
    }
}
