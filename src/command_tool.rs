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
use crate::platform::sender::PlatformSender;
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult};

pub struct CommandTool {
    sandbox_dir: PathBuf,
    cancel_registry: Arc<CancelRegistry>,
    sender: Arc<dyn PlatformSender>,
}

impl CommandTool {
    pub fn new(
        sandbox_dir: PathBuf,
        cancel_registry: Arc<CancelRegistry>,
        sender: Arc<dyn PlatformSender>,
    ) -> Self {
        Self {
            sandbox_dir,
            cancel_registry,
            sender,
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

        let mut child = TokioCommand::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.sandbox_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .spawn()?;

        let escaped_cmd = crate::utils::telegram_markdown::escape_text(command);

        let status_text = format!("💻 Running: `{}`\n\n```\n⏳ Starting...\n```", escaped_cmd);
        let msg_id = self
            .sender
            .show_cancel_button(&ctx.chat_id, &status_text, &cmd_id)
            .await?;

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
                    if last_edit.elapsed() >= std::time::Duration::from_millis(500) {
                        let capped = crate::utils::strings::truncate_tail(&output_buffer, 3500);
                        let text = format!("💻 Running: `{}`\n\n```\n{}\n```", escaped_cmd, capped);
                        if let Err(e) = self.sender.edit_message(&ctx.chat_id, &msg_id, &text).await {
                            warn!("Failed to update running message: {e}");
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
            let body = format_body(&output_buffer, "");
            let text = match body {
                None => format!("❌ Cancelled: `{}`", escaped_cmd),
                Some(b) => format!("❌ Cancelled: `{}`\n\n{}", escaped_cmd, b),
            };
            let _ = self.sender.edit_message(&ctx.chat_id, &msg_id, &text).await;
            "⚠️ User cancelled the command".to_string()
        } else if let Some(code) = exit_code {
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
            let _ = self.sender.edit_message(&ctx.chat_id, &msg_id, &text).await;
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
