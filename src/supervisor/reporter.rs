use crate::supervisor::job::Job;

pub struct Reporter;

impl Reporter {
    pub fn render(jobs: &[Job]) -> String {
        let mut out = String::new();
        for j in jobs {
            out.push_str(&format!("• [{}] {}\n", j.backend, j.goal));
            if let Some(res) = &j.result {
                if !res.summary.is_empty() {
                    out.push_str("  ");
                    out.push_str(&res.summary);
                    out.push('\n');
                }
                if !res.changed_files.is_empty() {
                    out.push_str("  changed files:\n");
                    for f in &res.changed_files {
                        out.push_str(&format!("    - {f}\n"));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reporter_renders_human_summary() {
        use crate::supervisor::job::*;
        let mut j = Job::new("t", JobType::ExecutorJob, "reasoning", "g");
        j.status = JobStatus::Succeeded;
        j.result = Some(JobOutput {
            status: JobStatus::Succeeded,
            summary: "All good.".into(),
            evidence: vec![Evidence::ExitCode { code: 0 }],
            errors: vec![],
            changed_files: vec!["src/foo.rs".into()],
            next_step: None,
        });
        let r = Reporter::render(&[j]);
        assert!(r.contains("All good."));
        assert!(r.contains("src/foo.rs"));
    }
}
