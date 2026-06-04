use rustfox::config::Config;

#[test]
fn sandbox_defaults_to_home_workspace_and_excludes_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join(".rustfox");
    let cfg_path = tmp.path().join("config.toml");
    let toml = format!(
        r#"
        [telegram]
        bot_token = "tok"
        allowed_user_ids = [1]
        [openrouter]
        api_key = "key"
        [general]
        home = "{}"
        "#,
        home.display()
    );
    std::fs::write(&cfg_path, toml).unwrap();
    let cfg = Config::load(&cfg_path).unwrap();

    // Sandbox is the workspace subdir of home.
    assert_eq!(cfg.sandbox.allowed_directory, home.join("workspace"));
    // DB lives ABOVE the sandbox → structurally unreachable by file tools.
    assert_eq!(cfg.memory.database_path, home.join("rustfox.db"));
    assert!(!cfg
        .memory
        .database_path
        .starts_with(&cfg.sandbox.allowed_directory));
}
