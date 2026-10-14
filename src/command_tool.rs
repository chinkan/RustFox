use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::process::Command as TokioCommand;
use tracing::warn;

use crate::cancel_registry::CancelRegistry;
use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult, ToolUiMode};

/// Controls how command execution messages are sent to Telegram.
enum SendMode {
    /// Full live output with cancel button.
    Verbose,
    /// Cancel button only, no live edits. Message deleted on completion.
    Minimal,
    /// No message sent. Tool notifier handles nothing (silent mode).
    Silent,
}

pub struct CommandTool {
    sandbox_dir: PathBuf,
    /// Live mode (portal can switch without restart via shared lock).
    mode: Arc<std::sync::RwLock<crate::sandbox::SandboxMode>>,
    cancel_registry: Arc<CancelRegistry>,
    /// Optional Slice 3 bridge: inject `secret_env` names into child env.
    secret_bridge: Option<Arc<crate::secret_store::SecretBridge>>,
    secret_env_names: Vec<String>,
}

impl CommandTool {
    pub fn new(sandbox_dir: PathBuf, cancel_registry: Arc<CancelRegistry>) -> Self {
        Self {
            sandbox_dir,
            mode: Arc::new(std::sync::RwLock::new(crate::sandbox::SandboxMode::Trusted)),
            cancel_registry,
            secret_bridge: None,
            secret_env_names: Vec::new(),
        }
    }

    /// Set the effective `[sandbox] mode` (and share the lock with the portal).
    pub fn with_mode(mut self, mode: crate::sandbox::SandboxMode) -> Self {
        self.mode = Arc::new(std::sync::RwLock::new(mode));
        self
    }

    /// Share an existing mode lock (portal live-switch).
    pub fn with_mode_handle(
        mut self,
        mode: Arc<std::sync::RwLock<crate::sandbox::SandboxMode>>,
    ) -> Self {
        self.mode = mode;
        self
    }

    pub fn mode_handle(&self) -> Arc<std::sync::RwLock<crate::sandbox::SandboxMode>> {
        Arc::clone(&self.mode)
    }

    /// Attach SecretBridge + names to inject into spawn env (Slice 3).
    pub fn with_secrets(
        mut self,
        bridge: Arc<crate::secret_store::SecretBridge>,
        names: Vec<String>,
    ) -> Self {
        self.secret_bridge = Some(bridge);
        self.secret_env_names = names;
        self
    }
}

#[async_trait]
impl ToolHandler for CommandTool {
    fn define(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "execute_command".to_string(),
                description: "Execute a shell command within the sandbox directory.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The shell command to execute" }
                    },
                    "required": ["command"]
                }),
            },
        }]
    }

    async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult {
        match name {
            "execute_command" => self.exec_command(&args, &ctx).await,
            _ => anyhow::bail!("CommandTool: unknown tool {name}"),
        }
    }
}

impl CommandTool {
    /// Resolve `[sandbox].secret_env` via SecretBridge for child-process inject.
    /// On missing secret returns a safe name-only error string (pending+notify
    /// already fired inside `env_for_names` / `require`).
    fn resolve_spawn_secret_env(
        &self,
    ) -> Result<std::collections::HashMap<String, String>, String> {
        let Some(ref bridge) = self.secret_bridge else {
            return Ok(std::collections::HashMap::new());
        };
        if self.secret_env_names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        bridge
            .env_for_names(&self.secret_env_names)
            .map_err(|e| e.to_string())
    }

    async fn exec_command(&self, arguments: &Value, ctx: &ToolContext) -> ToolResult {
        let command = arguments["command"]
            .as_str()
            .context("Missing 'command' argument")?;
        let cmd_id = format!("cmd_{}", uuid::Uuid::new_v4());

        // Slice 3: inject required secrets into child env only (never log values).
        let secret_env = match self.resolve_spawn_secret_env() {
            Ok(map) => map,
            Err(safe_msg) => return Ok(safe_msg),
        };

        let mode = *self.mode.read().unwrap_or_else(|e| e.into_inner());

        let mut cmd = if mode == crate::sandbox::SandboxMode::Sandbox {
            // Windows: refuse. Linux/macOS: OS sandbox. Fail closed — never
            // silently fall back to trusted (ADR-0025 §6).
            #[cfg(windows)]
            {
                return Ok(crate::sandbox::MSG_WINDOWS_SANDBOX.to_string());
            }
            #[cfg(not(windows))]
            {
                match crate::sandbox::prepare_sandboxed_command(&self.sandbox_dir, command) {
                    Ok(c) => c,
                    Err(crate::sandbox::SandboxStartError::WindowsUnavailable) => {
                        return Ok(crate::sandbox::MSG_WINDOWS_SANDBOX.to_string());
                    }
                    Err(_) => {
                        return Ok(crate::sandbox::MSG_SANDBOX_FAILED.to_string());
                    }
                }
            }
        } else {
            let mut c = TokioCommand::new(if cfg!(windows) { "cmd" } else { "sh" });
            c.arg(if cfg!(windows) { "/C" } else { "-c" })
                .arg(command)
                .current_dir(&self.sandbox_dir);
            c
        };
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (k, v) in &secret_env {
            cmd.env(k, v);
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) if mode == crate::sandbox::SandboxMode::Sandbox => {
                warn!("sandbox spawn failed (fail-closed): {e}");
                return Ok(crate::sandbox::MSG_SANDBOX_FAILED.to_string());
            }
            Err(e) => return Err(e.into()),
        };

        let escaped_cmd = crate::utils::telegram_markdown::escape_text(command);

        // Verbose: cancel button + live output + final result
        // Minimal: cancel button (simple text) + no live output, delete on finish
        // Silent: no message at all (tool_notifier handles nothing)
        let (msg_id, send_mode) = match ctx.tool_ui_mode {
            ToolUiMode::Verbose => {
                let status_text =
                    format!("💻 Running: `{}`\n\n```\n⏳ Starting...\n```", escaped_cmd);
                let id = ctx
                    .sender
                    .show_cancel_button(&ctx.chat_id, &status_text, &cmd_id)
                    .await?;
                (Some(id), SendMode::Verbose)
            }
            ToolUiMode::Minimal => {
                let status_text = format!("⏳ Running: `{}`", escaped_cmd);
                let id = ctx
                    .sender
                    .show_cancel_button(&ctx.chat_id, &status_text, &cmd_id)
                    .await?;
                (Some(id), SendMode::Minimal)
            }
            ToolUiMode::Silent => (None, SendMode::Silent),
        };

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        self.cancel_registry
            .register(cmd_id.clone(), cancel_tx)
            .await;

        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel::<String>(256);
        let output_tx2 = output_tx.clone();
        let mut child_stdout = child.stdout.take();
        let mut child_stderr = child.stderr.take();

        let stdout_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            while let Some(stream) = child_stdout.as_mut() {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let _ = output_tx
                            .send(String::from_utf8_lossy(&buf[..n]).to_string())
                            .await;
                    }
                }
            }
        });

        let stderr_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            while let Some(stream) = child_stderr.as_mut() {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let _ = output_tx2
                            .send(String::from_utf8_lossy(&buf[..n]).to_string())
                            .await;
                    }
                }
            }
        });

        const MAX_BUFFER_CHARS: usize = 100_000;
        let mut output_buffer = String::new();
        let mut last_edit = Instant::now();
        let mut exit_code: Option<i32> = None;
        let mut cancelled = false;
        tokio::pin!(cancel_rx);

        loop {
            tokio::select! {
                Some(chunk) = output_rx.recv() => {
                    output_buffer.push_str(&chunk);
                    if output_buffer.chars().count() > MAX_BUFFER_CHARS {
                        output_buffer = crate::utils::strings::truncate_tail(&output_buffer, MAX_BUFFER_CHARS);
                    }
                    if matches!(send_mode, SendMode::Verbose) && last_edit.elapsed() >= std::time::Duration::from_millis(500) {
                        let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                        let text = format!("💻 Running: `{}`\n\n```\n{}\n```", escaped_cmd, capped);
                        if let Some(mid) = &msg_id {
                            if let Err(e) = ctx.sender.edit_message(&ctx.chat_id, mid, &text).await {
                                warn!("Failed to update running message: {e}");
                            }
                        }
                        last_edit = Instant::now();
                    }
                }
                status = child.wait() => {
                    exit_code = Some(status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1));
                    break;
                }
                _ = &mut cancel_rx => {
                    cancelled = true;
                    #[cfg(unix)]
                    if let Some(pid) = child.id() {
                        let _ = nix::sys::signal::killpg(
                            nix::unistd::Pid::from_raw(pid as i32),
                            nix::sys::signal::Signal::SIGKILL,
                        );
                    }
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    break;
                }
            }
        }

        let _ = tokio::join!(stdout_handle, stderr_handle);
        while let Ok(chunk) = output_rx.try_recv() {
            output_buffer.push_str(&chunk);
        }
        if output_buffer.chars().count() > MAX_BUFFER_CHARS {
            output_buffer = crate::utils::strings::truncate_tail(&output_buffer, MAX_BUFFER_CHARS);
        }
        if let Some(ref bridge) = self.secret_bridge {
            output_buffer = bridge.redact(&output_buffer);
        }

        fn format_body(buf: &str, no_output_msg: &str) -> Option<String> {
            if buf.is_empty() {
                if no_output_msg.is_empty() {
                    None
                } else {
                    Some(no_output_msg.to_owned())
                }
            } else {
                let capped = crate::utils::strings::truncate_tail(buf, 3500);
                Some(format!("```\n{}\n```", capped))
            }
        }

        let result = if cancelled {
            if let Some(mid) = &msg_id {
                match send_mode {
                    SendMode::Verbose => {
                        let body = format_body(&output_buffer, "");
                        let text = match body {
                            None => format!("❌ Cancelled: `{}`", escaped_cmd),
                            Some(b) => format!("❌ Cancelled: `{}`\n\n{}", escaped_cmd, b),
                        };
                        let _ = ctx.sender.edit_message(&ctx.chat_id, mid, &text).await;
                    }
                    SendMode::Minimal => {
                        // Delete the minimal message
                        let _ = ctx.sender.delete_message(&ctx.chat_id, mid).await;
                    }
                    // Silent mode sends no message; nothing to clean up.
                    SendMode::Silent => {}
                }
            }
            "⚠️ User cancelled the command".to_string()
        } else if let Some(code) = exit_code {
            if let Some(mid) = &msg_id {
                match send_mode {
                    SendMode::Verbose => {
                        let (icon, label) = if code == 0 {
                            ("✅", "Completed")
                        } else {
                            ("❌", "Failed")
                        };
                        let body = format_body(&output_buffer, "Command completed with no output.");
                        let text = format!(
                            "{} {}: `{}`\n\n{}",
                            icon,
                            label,
                            escaped_cmd,
                            body.unwrap_or_default()
                        );
                        let _ = ctx.sender.edit_message(&ctx.chat_id, mid, &text).await;
                    }
                    SendMode::Minimal => {
                        // Delete the minimal message
                        let _ = ctx.sender.delete_message(&ctx.chat_id, mid).await;
                    }
                    // Silent mode sends no message; nothing to clean up.
                    SendMode::Silent => {}
                }
            }
            let mut result = String::new();
            if !output_buffer.is_empty() {
                result.push_str(output_buffer.trim_end());
                result.push('\n');
            }
            result.push_str(&format!("Exit code: {}", code));
            result
        } else {
            unreachable!()
        };

        self.cancel_registry.unregister(&cmd_id).await;
        // Slice 3: never return raw secret values to LLM / tool echoes.
        let result = if let Some(ref bridge) = self.secret_bridge {
            bridge.redact(&result)
        } else {
            result
        };
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel_registry::CancelRegistry;
    use crate::platform::sender::{MessageFormat, PlatformMessageId, PlatformSender};
    use crate::secret_store::{FakeSecretStore, PendingSecretRegistry, SecretBridge};
    use crate::tool_registry::ToolUiMode;
    use async_trait::async_trait;
    use serde_json::json;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    struct NopSender;

    #[async_trait]
    impl PlatformSender for NopSender {
        async fn send_message(
            &self,
            _chat_id: &str,
            _text: &str,
            _format: MessageFormat,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn send_file(
            &self,
            _chat_id: &str,
            _path: &Path,
            _caption: Option<&str>,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn show_cancel_button(
            &self,
            _chat_id: &str,
            _text: &str,
            _cancel_id: &str,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &PlatformMessageId,
            _text: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete_message(
            &self,
            _chat_id: &str,
            _message_id: &PlatformMessageId,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn notify_shutdown(&self, _chat_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn ctx(sandbox: &Path) -> ToolContext {
        ToolContext {
            sandbox_dir: sandbox.to_path_buf(),
            home_dir: None,
            sender: Arc::new(NopSender),
            cancel_registry: Arc::new(CancelRegistry::new()),
            user_id: "u".into(),
            chat_id: "1".into(),
            bot_id: crate::platform::DEFAULT_BOT_ID.into(),
            tool_ui_mode: ToolUiMode::Silent,
        }
    }

    #[tokio::test]
    async fn execute_command_injects_secret_env_into_child() {
        let dir = tempdir().unwrap();
        let store = FakeSecretStore::with_secrets([("MY_SECRET", "inject-me-VALUE-zz9")]).unwrap();
        let pending = Arc::new(PendingSecretRegistry::default());
        let bridge = Arc::new(SecretBridge::new(Arc::new(store), pending));
        let tool = CommandTool::new(dir.path().to_path_buf(), Arc::new(CancelRegistry::new()))
            .with_secrets(bridge.clone(), vec!["MY_SECRET".into()]);

        // Write env value to a file so we can assert injection even after result redaction.
        let secret_cmd = if cfg!(windows) {
            "echo %MY_SECRET%> secret_out.txt"
        } else {
            "printf '%s' \"$MY_SECRET\" > secret_out.txt"
        };
        let result = tool
            .execute(
                "execute_command",
                json!({ "command": secret_cmd }),
                ctx(dir.path()),
            )
            .await
            .unwrap();
        let written = std::fs::read_to_string(dir.path().join("secret_out.txt")).unwrap();
        assert_eq!(written.trim_end(), "inject-me-VALUE-zz9");
        // Tool result / echoes must not leak the raw value.
        assert!(
            !result.contains("inject-me-VALUE-zz9"),
            "result leaked secret: {result}"
        );
    }

    #[tokio::test]
    async fn execute_command_missing_secret_safe_error_and_notify() {
        let dir = tempdir().unwrap();
        let store = FakeSecretStore::new();
        let pending = Arc::new(PendingSecretRegistry::default());
        let bridge = Arc::new(SecretBridge::new(Arc::new(store), Arc::clone(&pending)));
        let notifies = Arc::new(AtomicUsize::new(0));
        let names = Arc::new(Mutex::new(Vec::<String>::new()));
        let n2 = Arc::clone(&notifies);
        let names2 = Arc::clone(&names);
        bridge.set_notify(Arc::new(move |name, url| {
            n2.fetch_add(1, Ordering::SeqCst);
            names2.lock().unwrap().push(format!("{name}|{url}"));
        }));

        let tool = CommandTool::new(dir.path().to_path_buf(), Arc::new(CancelRegistry::new()))
            .with_secrets(bridge, vec!["MISSING_KEY".into()]);

        let result = tool
            .execute(
                "execute_command",
                json!({ "command": "echo should-not-run" }),
                ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(
            result.contains("MISSING_KEY"),
            "safe error should name the secret: {result}"
        );
        assert!(
            !result.to_lowercase().contains("sk-") && !result.contains("VALUE"),
            "error must not leak a secret value: {result}"
        );
        assert_eq!(notifies.load(Ordering::SeqCst), 1);
        assert!(!pending.list().is_empty());
        assert!(!dir.path().join("should-not-run").exists());
    }

    struct CountingSender {
        hits: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl PlatformSender for CountingSender {
        async fn send_message(
            &self,
            _: &str,
            _: &str,
            _: MessageFormat,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn send_file(
            &self,
            _: &str,
            _: &Path,
            _: Option<&str>,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("0:1".into())
        }
        async fn show_cancel_button(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> anyhow::Result<PlatformMessageId> {
            self.hits.fetch_add(1, Ordering::SeqCst);
            Ok("9:1".into())
        }
        async fn edit_message(
            &self,
            _: &str,
            _: &PlatformMessageId,
            _: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete_message(&self, _: &str, _: &PlatformMessageId) -> anyhow::Result<()> {
            Ok(())
        }
        async fn notify_shutdown(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn execute_command_status_uses_call_context_sender() {
        let dir = tempdir().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let mut context = ctx(dir.path());
        context.tool_ui_mode = ToolUiMode::Minimal;
        context.sender = Arc::new(CountingSender {
            hits: Arc::clone(&hits),
        });
        let tool = CommandTool::new(dir.path().to_path_buf(), Arc::new(CancelRegistry::new()));
        let result = tool
            // `exit 0` works under both `sh -c` and `cmd /C`.
            .execute("execute_command", json!({ "command": "exit 0" }), context)
            .await
            .unwrap();
        assert!(result.contains("Exit code: 0"), "{result}");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "tool status must go through ToolContext.sender (the owning bot)"
        );
    }

    #[test]
    fn resolve_spawn_secret_env_empty_when_no_bridge() {
        let tool = CommandTool::new(PathBuf::from("."), Arc::new(CancelRegistry::new()));
        let map = tool.resolve_spawn_secret_env().unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn sandbox_mode_windows_message_exact() {
        assert_eq!(
            crate::sandbox::MSG_WINDOWS_SANDBOX,
            "Running commands is not available in safe mode on Windows yet. You can switch to trusted mode in the portal."
        );
    }

    #[test]
    fn sandbox_failed_message_exact() {
        assert_eq!(
            crate::sandbox::MSG_SANDBOX_FAILED,
            "Running commands is not available in safe mode on this computer right now. You can switch to trusted mode in the portal. If you run RustFox in Docker, allow sandboxing in your Docker settings."
        );
    }

    #[tokio::test]
    async fn sandbox_mode_fail_closed_when_bwrap_missing() {
        // Force missing bwrap via env override to a nonexistent path.
        // Shared lock: other tests probe bwrap through the same env var.
        let _bwrap =
            crate::sandbox::test_env::ForcedBwrapPath::set("/nonexistent/rustfox-no-bwrap");
        let dir = tempdir().unwrap();
        let tool = CommandTool::new(dir.path().to_path_buf(), Arc::new(CancelRegistry::new()))
            .with_mode(crate::sandbox::SandboxMode::Sandbox);
        let result = tool
            .execute(
                "execute_command",
                json!({ "command": "echo should-not-run" }),
                ctx(dir.path()),
            )
            .await
            .unwrap();
        #[cfg(windows)]
        {
            assert_eq!(result, crate::sandbox::MSG_WINDOWS_SANDBOX);
        }
        #[cfg(not(windows))]
        {
            assert_eq!(result, crate::sandbox::MSG_SANDBOX_FAILED);
            assert!(!dir.path().join("should-not-run").exists());
        }
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn windows_sandbox_mode_returns_canonical_error() {
        let dir = tempdir().unwrap();
        let tool = CommandTool::new(dir.path().to_path_buf(), Arc::new(CancelRegistry::new()))
            .with_mode(crate::sandbox::SandboxMode::Sandbox);
        let result = tool
            .execute(
                "execute_command",
                json!({ "command": "echo hi" }),
                ctx(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(result, crate::sandbox::MSG_WINDOWS_SANDBOX);
    }
}
