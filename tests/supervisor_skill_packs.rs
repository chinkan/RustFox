#[tokio::test]
async fn ships_five_supervisor_skill_packs() {
    let skills = rustfox::skills::loader::load_skills_from_dir(
        std::path::Path::new("skills"),
        std::path::PathBuf::from("skills"),
    )
    .await
    .unwrap();
    for n in [
        "sup-coding",
        "sup-research",
        "sup-writing",
        "sup-ops",
        "sup-general",
    ] {
        let s = skills.get(n).unwrap_or_else(|| panic!("missing {n}"));
        assert!(
            s.supervisor_workflow.is_some(),
            "{n} missing supervisor_workflow"
        );
    }
}
