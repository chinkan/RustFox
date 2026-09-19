use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command as TokioCommand;
use tracing::warn;

use crate::cancel_registry::CancelRegistry;
use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::platform::sender::PlatformSender;
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult, ToolUiMode};

enum SendMode {
    Verbose,
    Minimal,
    Silent,
}

pub struct CommandTool {
    sandbox_dir: PathBuf,
    cancel_registry: Arc<CancelRegistry>,
    sender: Arc<dyn PlatformSender>,
    execute_timeout_secs: u64,
}

impl CommandTool {
    pub fn new(
        sandbox_dir: PathBuf,
        cancel_registry: Arc<CancelRegistry>,
        sender: Arc<dyn PlatformSender>,
        execute_timeout_secs: u64,
    ) -> Self {
        Self {
            sandbox_dir,
            cancel_registry,
            sender,
            execute_timeout_secs,
        }
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
    async fn exec_command(&self, arguments: &Value, ctx: &ToolContext) -> ToolResult {
        let command = arguments["command"]
            .as_str()
            .context("Missing 'command' argument")?;
        let cmd_id = format!("cmd_{}", uuid::Uuid::new_v4());

        let mut cmd = TokioCommand::new(if cfg!(windows) { "cmd" } else { "sh" });
        cmd.arg(if cfg!(windows) { "/C" } else { "-c" })
            .arg(command)
            .current_dir(&self.sandbox_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn()?;

        let escaped_cmd = crate::utils::telegram_markdown::escape_text(command);

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        self.cancel_registry
            .register(cmd_id.clone(), cancel_tx)
            .await;
        // Verbose: cancel button + live output + final result
        // Minimal: cancel button (simple text) + no live output, delete on finish
        // Silent: no message at all (tool_notifier handles nothing)
        let (msg_id, send_mode) = match ctx.tool_ui_mode {
            ToolUiMode::Verbose => {
                let st = format!("💻 Running: `{}`\n\n```\n⏳ Starting...\n```", escaped_cmd);
                let id = self
                    .sender
                    .show_cancel_button(&ctx.chat_id, &st, &cmd_id)
                    .await?;
                (Some(id), SendMode::Verbose)
            }
            ToolUiMode::Minimal => {
                let st = format!("⏳ Running: `{}`", escaped_cmd);
                let id = self
                    .sender
                    .show_cancel_button(&ctx.chat_id, &st, &cmd_id)
                    .await?;
                (Some(id), SendMode::Minimal)
            }
            ToolUiMode::Silent => (None, SendMode::Silent),
        };

        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel::<String>(256);
        let stdout_tx = output_tx.clone();
        let stderr_tx = output_tx.clone();
        let child_stdout = child.stdout.take();
        let child_stderr = child.stderr.take();

        let stdout_handle = tokio::spawn(async move {
            if let Some(reader) = child_stdout {
                crate::utils::process::drain_pipe(reader, stdout_tx).await;
            }
        });
        let stderr_handle = tokio::spawn(async move {
            if let Some(reader) = child_stderr {
                crate::utils::process::drain_pipe(reader, stderr_tx).await;
            }
        });

        let mut output_buffer = String::new();
        let mut last_edit = Instant::now();
        let mut exit_code: Option<i32> = None;
        let mut cancelled = false;
        let mut timed_out = false;
        tokio::pin!(cancel_rx);

        let timeout_secs = self.execute_timeout_secs;
        let timeout_fut = crate::utils::process::optional_timeout(timeout_secs);
        tokio::pin!(timeout_fut);

        loop {
            tokio::select! {
                Some(chunk) = output_rx.recv() => {
                    output_buffer.push_str(&chunk);
                    if output_buffer.chars().count() > crate::utils::process::OUTPUT_BUFFER_CHARS {
                        output_buffer = crate::utils::strings::truncate_tail(
                            &output_buffer,
                            crate::utils::process::OUTPUT_BUFFER_CHARS,
                        );
                    }
                    if matches!(send_mode, SendMode::Verbose) && last_edit.elapsed() >= Duration::from_millis(500) {
                        let capped = crate::utils::strings::truncate_tail(
                            &output_buffer,
                            crate::utils::process::OUTPUT_SNIPPET_CHARS,
                        );
                        let text = format!("💻 Running: `{}`\n\n```\n{}\n```", escaped_cmd, capped);
                        if let Some(mid) = &msg_id {
                            if let Err(e) = self.sender.edit_message(&ctx.chat_id, mid, &text).await {
                                warn!("Failed to update running message: {e}");
                            }
                        }
                        last_edit = Instant::now();
                    }
                }
                status = child.wait() => {
                    exit_code = Some(match status {
                        Ok(s) => s.code().unwrap_or(-1),
                        Err(_) => -1,
                    });
                    break;
                }
                _ = &mut cancel_rx => {
                    cancelled = true;
                    crate::utils::process::kill_child(&mut child).await;
                    break;
                }
                _ = &mut timeout_fut => {
                    timed_out = true;
                    crate::utils::process::kill_child(&mut child).await;
                    break;
                }
            }
        }

        // Abort orphan drain tasks (daemon holding pipe) — same as supervisor shell.
        crate::utils::process::finish_drains(
            vec![stdout_handle, stderr_handle],
            crate::utils::process::DRAIN_JOIN_TIMEOUT,
        )
        .await;

        while let Ok(chunk) = output_rx.try_recv() {
            output_buffer.push_str(&chunk);
        }
        if output_buffer.chars().count() > crate::utils::process::OUTPUT_BUFFER_CHARS {
            output_buffer = crate::utils::strings::truncate_tail(
                &output_buffer,
                crate::utils::process::OUTPUT_BUFFER_CHARS,
            );
        }

        fn format_body(buf: &str, no_output_msg: &str) -> Option<String> {
            if buf.is_empty() {
                if no_output_msg.is_empty() {
                    None
                } else {
                    Some(no_output_msg.to_owned())
                }
            } else {
                let capped = crate::utils::strings::truncate_tail(
                    buf,
                    crate::utils::process::OUTPUT_SNIPPET_CHARS,
                );
                Some(format!("```\n{}\n```", capped))
            }
        }

        let result = if cancelled || timed_out {
            let label = if timed_out {
                format!("Timed out after {}s", timeout_secs)
            } else {
                "Cancelled".to_string()
            };
            if let Some(mid) = &msg_id {
                match send_mode {
                    SendMode::Verbose => {
                        let body = format_body(&output_buffer, "");
                        let text = match body {
                            None => format!("❌ {}: `{}`", label, escaped_cmd),
                            Some(b) => format!("❌ {}: `{}`\n\n{}", label, escaped_cmd, b),
                        };
                        if let Err(e) = self.sender.edit_message(&ctx.chat_id, mid, &text).await {
                            warn!("Failed to edit cancel/timeout message: {e}");
                        }
                    }
                    SendMode::Minimal => {
                        if let Err(e) = self.sender.delete_message(&ctx.chat_id, mid).await {
                            warn!("Failed to delete minimal command message: {e}");
                        }
                    }
                    SendMode::Silent => {}
                }
            }
            let mut msg = if timed_out {
                format!("⚠️ Command timed out after {}s", timeout_secs)
            } else {
                "⚠️ User cancelled the command".to_string()
            };
            if !output_buffer.is_empty() {
                let capped = crate::utils::strings::truncate_tail(
                    &output_buffer,
                    crate::utils::process::OUTPUT_SNIPPET_CHARS,
                );
                msg.push_str(&format!("\n\nPartial output:\n```\n{}\n```", capped));
            }
            msg
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
                        if let Err(e) = self.sender.edit_message(&ctx.chat_id, mid, &text).await {
                            warn!("Failed to edit completed command message: {e}");
                        }
                    }
                    SendMode::Minimal => {
                        if let Err(e) = self.sender.delete_message(&ctx.chat_id, mid).await {
                            warn!("Failed to delete minimal command message: {e}");
                        }
                    }
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
        Ok(result)
    }
}
