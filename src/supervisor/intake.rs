use crate::supervisor::task::Task;

pub struct IntakeRouter;

impl IntakeRouter {
    pub fn normalize(raw: &str) -> Task {
        let trimmed = raw.trim();
        let first_line = trimmed.lines().next().unwrap_or(trimmed);
        let title: String = first_line.chars().take(80).collect();
        Task::new(&title, trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intake_uses_first_line_as_title_and_full_text_as_request() {
        let task = IntakeRouter::normalize("Fix the login bug\nthe button does nothing");
        assert_eq!(task.title, "Fix the login bug");
        assert_eq!(
            task.user_request,
            "Fix the login bug\nthe button does nothing"
        );
        assert_eq!(task.status, crate::supervisor::task::TaskStatus::Intake);
        assert!(!task.id.is_empty());
    }

    #[test]
    fn intake_truncates_long_titles_to_80_chars() {
        let long = "A".repeat(200);
        let task = IntakeRouter::normalize(&long);
        assert!(task.title.len() <= 80);
    }
}
