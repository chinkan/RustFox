pub enum Action {
    Install,
    Remove,
    Status,
    Start,
    Stop,
}
pub fn handle(_action: Action) -> anyhow::Result<()> {
    anyhow::bail!("Service commands not yet implemented")
}
