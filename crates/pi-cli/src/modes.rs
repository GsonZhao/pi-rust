//! Output modes. Mirrors the v1-relevant slice of the TS
//! `packages/coding-agent/src/modes/{print-mode,json-event,rpc-mode}.ts` — the
//! three run shapes a harness-backed CLI needs:
//!
//! - [`print`] — single-shot: send the prompt(s), print the final assistant
//!   text (or the error) to stdout, exit. Mirrors TS `runPrintMode` (text mode).
//! - [`json`] — single-shot streaming: emit each harness event as a JSON line
//!   on stdout, then the final outcome. Mirrors TS `runPrintMode`
//!   (`mode === "json"`) + [`json_event::toJsonEvent`].
//! - [`interactive`] — a minimal line-oriented REPL: read prompts from stdin,
//!   run each, print the assistant text, loop until EOF / `/exit`. v1 does NOT
//!   port the TS `InteractiveMode` TUI (`modes/interactive/*` — a full terminal
//!   UI with Ink/React components); this is a deliberately minimal replacement,
//!   documented in `docs/m6-cli-open-questions.md`.
//!
//! All three drive the same `AgentHarness` via `AgentLane::prompt_text`.

use std::io::{BufRead, IsTerminal, Write};
use std::sync::{Arc, Mutex};

use rpi_agent::events::AgentEvent;
use rpi_ai::types::{AssistantMessage, Content, ImageContent, StopReason};
use rpi_harness::agent_harness::{AgentHarness, AgentLane, HarnessRunOutcome};
use rpi_harness::events::{HarnessEvent, RunEndOutcome};

use crate::args::Args;

/// Extract the concatenated text content from an assistant message. Mirrors the
/// TS print-mode loop (`for content of assistantMsg.content if type===text`).
pub fn assistant_text(msg: &AssistantMessage) -> String {
    msg.content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect()
}

/// The exit code a run's outcome maps to. Mirrors TS print mode: error/aborted
/// ⇒ exit 1; everything else ⇒ 0.
pub fn outcome_exit_code(outcome: &HarnessRunOutcome) -> i32 {
    match outcome {
        HarnessRunOutcome::Failed { .. } | HarnessRunOutcome::Aborted { .. } => 1,
        _ => 0,
    }
}

/// `print` mode: send the initial message (prompt text + inline `@file`
/// expansions), then any follow-up messages, print the final assistant text,
/// return the exit code. Mirrors TS `runPrintMode` (text).
pub async fn print(
    harness: &AgentHarness,
    _args: &Args,
    initial: Option<String>,
    extra_messages: &[String],
    initial_images: Vec<ImageContent>,
) -> i32 {
    let lane: Arc<dyn AgentLane> = harness.lane("main");

    let mut last_exit = 0;
    let mut last_msg: Option<AssistantMessage> = None;

    // The initial prompt (and its `@file` attachments) go in one user message;
    // extra positionals are separate prompts (mirrors the TS loop).
    let mut prompts: Vec<String> = Vec::new();
    if let Some(init) = initial {
        prompts.push(init);
    }
    for m in extra_messages {
        prompts.push(m.clone());
    }

    if prompts.is_empty() {
        // Nothing to do — print mode with no prompt is a no-op success.
        return 0;
    }

    let mut images = initial_images;
    for prompt in prompts {
        match lane.prompt_text(&prompt, std::mem::take(&mut images)).await {
            Ok(result) => {
                last_exit = outcome_exit_code(&result.outcome);
                match &result.outcome {
                    HarnessRunOutcome::Completed { final_message, .. }
                    | HarnessRunOutcome::Aborted { final_message, .. } => {
                        last_msg = Some(final_message.clone());
                    }
                    HarnessRunOutcome::Failed {
                        error,
                        final_message,
                        ..
                    } => {
                        if let Some(m) = final_message {
                            if m.stop_reason == StopReason::Error {
                                if let Some(em) = &m.error_message {
                                    eprintln!("{em}");
                                }
                            }
                        }
                        eprintln!("run failed: {error:?}");
                    }
                    HarnessRunOutcome::Suspended { .. } => {
                        eprintln!("run suspended (deferred) — resume is not supported in v1");
                        last_exit = 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("prompt rejected: {e}");
                return 1;
            }
        }
    }

    // Print the final assistant text to stdout (TS: writeRawStdout text + "\n").
    if let Some(m) = &last_msg {
        match m.stop_reason {
            StopReason::Error => {
                if let Some(em) = &m.error_message {
                    eprintln!("{em}");
                }
                last_exit = 1;
            }
            StopReason::Aborted => {
                eprintln!("request aborted");
                last_exit = 1;
            }
            _ => {
                let text = assistant_text(m);
                let mut out = std::io::stdout();
                let _ = out.write_all(text.as_bytes());
                if !text.ends_with('\n') {
                    let _ = out.write_all(b"\n");
                }
                let _ = out.flush();
            }
        }
    }

    last_exit
}

/// `json` mode: emit each harness event as a JSON line on stdout, run the
/// prompts, then emit a terminal `result` line carrying the outcome + final
/// text. Mirrors TS `runPrintMode` (`mode === "json"`) streaming every event.
pub async fn json(
    harness: &AgentHarness,
    _args: &Args,
    initial: Option<String>,
    extra_messages: &[String],
    initial_images: Vec<ImageContent>,
    mut agent_events: Option<tokio::sync::broadcast::Receiver<AgentEvent>>,
) -> i32 {
    let lane: Arc<dyn AgentLane> = harness.lane("main");
    let collected: Arc<Mutex<Vec<HarnessEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_for_watch = collected.clone();

    // A watch captures every event (RunStart fires inline during prompt_text,
    // before a post-call listener could attach — same reason as the M5g test).
    let mut watch = harness.events().watch(|| ());
    watch.start(Arc::new(move |event: &HarnessEvent| {
        // Emit each event live as JSON, and also buffer for the final summary.
        emit_json_event(event);
        collected_for_watch.lock().unwrap().push(event.clone());
    }));
    // Keep the watch alive for the whole run. Leaking is acceptable for a
    // single-shot CLI process (the bus outlives this scope anyway).
    std::mem::forget(watch);

    // The harness bus carries run lifecycle events; the agent receiver carries
    // the native fine-grained stream (turns, message deltas, and tools).
    let agent_event_task = agent_events.take().map(|mut rx| {
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => emit_agent_event(&event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    });

    let mut prompts: Vec<String> = Vec::new();
    if let Some(init) = initial {
        prompts.push(init);
    }
    for m in extra_messages {
        prompts.push(m.clone());
    }

    let mut last_exit = 0;
    let mut final_outcome: Option<HarnessRunOutcome> = None;

    let mut images = initial_images;
    for prompt in prompts {
        match lane.prompt_text(&prompt, std::mem::take(&mut images)).await {
            Ok(result) => {
                last_exit = outcome_exit_code(&result.outcome);
                final_outcome = Some(result.outcome);
            }
            Err(e) => {
                // Emit a structured error line + exit.
                let line = serde_json::json!({
                    "type": "error",
                    "error": e.to_string(),
                });
                println!("{line}");
                if let Some(task) = agent_event_task {
                    task.abort();
                }
                return 1;
            }
        }
    }

    // Terminal result summary.
    let (outcome_str, final_text) = match final_outcome {
        Some(HarnessRunOutcome::Completed { final_message, .. }) => {
            ("completed", Some(assistant_text(&final_message)))
        }
        Some(HarnessRunOutcome::Aborted { final_message, .. }) => {
            ("aborted", Some(assistant_text(&final_message)))
        }
        Some(HarnessRunOutcome::Failed { final_message, .. }) => {
            let t = final_message.as_ref().map(assistant_text);
            ("failed", t)
        }
        Some(HarnessRunOutcome::Suspended { .. }) => ("suspended", None),
        None => ("idle", None),
    };
    let result_line = serde_json::json!({
        "type": "result",
        "outcome": outcome_str,
        "finalText": final_text,
    });
    println!("{result_line}");
    if let Some(task) = agent_event_task {
        task.abort();
    }
    last_exit
}

fn emit_agent_event(event: &AgentEvent) {
    use rpi_ai::types::AssistantMessageEvent;
    let line = match event {
        AgentEvent::AgentStart => serde_json::json!({"type":"agent_start"}),
        AgentEvent::AgentEnd { messages } => serde_json::json!({
            "type":"agent_end", "messageCount": messages.len()
        }),
        AgentEvent::TurnStart => serde_json::json!({"type":"turn_start"}),
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => serde_json::json!({
            "type":"turn_end", "message": serde_json::to_value(message).ok(),
            "toolResultCount": tool_results.len()
        }),
        AgentEvent::MessageStart { message } => serde_json::json!({
            "type":"message_start", "message": serde_json::to_value(message).ok()
        }),
        AgentEvent::MessageEnd { message } => serde_json::json!({
            "type":"message_end", "message": serde_json::to_value(message).ok()
        }),
        AgentEvent::MessageUpdate {
            assistant_message_event,
            ..
        } => {
            let mut value = serde_json::json!({"type":"message_update"});
            let object = value.as_object_mut().expect("json object");
            match assistant_message_event {
                AssistantMessageEvent::TextDelta {
                    content_index,
                    delta,
                    ..
                }
                | AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta,
                    ..
                }
                | AssistantMessageEvent::ToolCallDelta {
                    content_index,
                    delta,
                    ..
                } => {
                    object.insert("contentIndex".into(), (*content_index).into());
                    object.insert("delta".into(), delta.clone().into());
                }
                _ => {}
            }
            value
        }
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => serde_json::json!({
            "type":"tool_execution_start", "toolCallId":tool_call_id,
            "toolName":tool_name, "args":args
        }),
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            ..
        } => serde_json::json!({
            "type":"tool_execution_update", "toolCallId":tool_call_id, "toolName":tool_name
        }),
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            is_error,
            ..
        } => serde_json::json!({
            "type":"tool_execution_end", "toolCallId":tool_call_id,
            "toolName":tool_name, "isError":is_error
        }),
    };
    println!("{line}");
}

/// Emit a single harness event as a JSON line on stdout. Mirrors the TS
/// `toJsonEvent` projection (here a lossy but stable shape: `type` + the event
/// payload's key fields).
fn emit_json_event(event: &HarnessEvent) {
    let line = match event {
        HarnessEvent::RunStart(e) => serde_json::json!({
            "type": "run_start",
            "lane": e.lane,
            "runId": e.run_id,
        }),
        HarnessEvent::RunEnd(e) => serde_json::json!({
            "type": "run_end",
            "lane": e.lane,
            "runId": e.run_id,
            "outcome": run_end_outcome_str(e.outcome),
            "leafId": e.leaf_id,
        }),
    };
    println!("{line}");
}

fn run_end_outcome_str(o: RunEndOutcome) -> &'static str {
    match o {
        RunEndOutcome::Completed => "completed",
        RunEndOutcome::Aborted => "aborted",
        RunEndOutcome::Failed => "failed",
    }
}

/// `interactive` mode: uses TUI if terminal supports it, falls back to minimal REPL.
///
/// `event_rx` carries the live `AgentEvent` stream (drained by the TUI to
/// render streaming responses). The REPL fallback ignores it.
///
/// `model_catalog` is the resolved provider's full model list, passed through
/// so the TUI's `/model` selector can display available models (read-only —
/// v1 does not switch models mid-session; see `docs/m6-cli-open-questions.md`).
pub async fn interactive(
    harness: &AgentHarness,
    event_rx: Option<tokio::sync::broadcast::Receiver<rpi_agent::AgentEvent>>,
    args: &Args,
    model_catalog: Vec<rpi_ai::Model>,
    initial: Option<String>,
    extra_messages: &[String],
    initial_images: Vec<ImageContent>,
    theme: Option<&str>,
    no_themes: bool,
    reload_context: &crate::session::ReloadContext,
) -> i32 {
    // Check if TUI is supported
    let force_tui = std::env::var("RPI_FORCE_TUI")
        .map(|v| v == "1")
        .unwrap_or(false);
    if force_tui || crate::interactive_tui::is_tui_supported() {
        // Use TUI-based interactive mode
        crate::interactive_tui::interactive_tui(
            harness,
            event_rx,
            args,
            model_catalog,
            initial,
            extra_messages,
            initial_images,
            theme,
            no_themes,
            reload_context,
        )
        .await
    } else {
        // Fall back to simple REPL
        interactive_repl(harness, args, initial, extra_messages, initial_images).await
    }
}

/// Simple REPL-based interactive mode (fallback for non-TTY environments).
pub async fn interactive_repl(
    harness: &AgentHarness,
    #[allow(unused_variables)] args: &Args,
    initial: Option<String>,
    extra_messages: &[String],
    initial_images: Vec<ImageContent>,
) -> i32 {
    // Debug: confirm we entered REPL mode
    let lane: Arc<dyn AgentLane> = harness.lane("main");
    let stdin = std::io::stdin();
    let is_tty = stdin.is_terminal();

    if is_tty {
        println!(
            "rpi interactive (v1 minimal REPL). Type /exit to quit, /abort to cancel a run.\n"
        );
    }

    // Run the initial prompt + extra messages first (same as print mode).
    let mut prompts: Vec<String> = Vec::new();
    if let Some(init) = initial {
        prompts.push(init);
    }
    for m in extra_messages {
        prompts.push(m.clone());
    }
    let mut images = initial_images;
    for prompt in prompts {
        if let Err(code) = run_one(&lane, &prompt, std::mem::take(&mut images)).await {
            return code;
        }
    }

    // Then read lines from stdin until EOF / `/exit`.
    let mut line = String::new();
    loop {
        if is_tty {
            print!("> ");
            let _ = std::io::stdout().flush();
        }
        line.clear();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "/exit" || trimmed == "/quit" {
            break;
        }
        if trimmed == "/abort" {
            let _ = lane.abort().await;
            eprintln!("(aborted)");
            continue;
        }
        if let Err(code) = run_one(&lane, trimmed, Vec::new()).await {
            return code;
        }
    }
    0
}

/// Run a single prompt in interactive mode, printing the assistant reply (or
/// the error). Returns `Ok(())` on success/soft-failure, `Err(exit_code)` on a
/// hard rejection.
async fn run_one(
    lane: &Arc<dyn AgentLane>,
    prompt: &str,
    images: Vec<ImageContent>,
) -> Result<(), i32> {
    match lane.prompt_text(prompt, images).await {
        Ok(result) => {
            match &result.outcome {
                HarnessRunOutcome::Completed { final_message, .. }
                | HarnessRunOutcome::Aborted { final_message, .. } => {
                    let text = assistant_text(final_message);
                    if !text.is_empty() {
                        println!("{text}");
                    }
                }
                HarnessRunOutcome::Failed {
                    error,
                    final_message,
                    ..
                } => {
                    if let Some(m) = final_message {
                        if let Some(em) = &m.error_message {
                            eprintln!("error: {em}");
                        }
                    }
                    eprintln!("run failed: {error:?}");
                }
                HarnessRunOutcome::Suspended { .. } => {
                    eprintln!("run suspended (deferred) — resume not supported in v1");
                }
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("prompt rejected: {e}");
            Err(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpi_ai::types::{
        AssistantMessage, Content, StopReason, TextContent, TextContentType, Usage,
    };
    use rpi_harness::session::types::OperationError;

    fn assistant(text: &str, stop: StopReason) -> AssistantMessage {
        AssistantMessage {
            role: rpi_ai::types::AssistantRole,
            content: vec![Content::Text(TextContent {
                kind: TextContentType,
                text: text.into(),
                text_signature: None,
            })],
            api: rpi_ai::Api::AnthropicMessages,
            provider: "anthropic".into(),
            model: "claude-sonnet-5".into(),
            response_model: None,
            response_id: None,
            usage: Usage::zero(),
            stop_reason: stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }
    }

    #[test]
    fn assistant_text_concatenates_text_blocks() {
        let m = assistant("hello", StopReason::Stop);
        assert_eq!(assistant_text(&m), "hello");
    }

    #[test]
    fn outcome_exit_code_maps_failed_aborted_to_1() {
        let failed = HarnessRunOutcome::Failed {
            leaf_id: "l".into(),
            error: OperationError {
                code: "boom".into(),
                message: "boom".into(),
            },
            final_entry_id: None,
            final_message: None,
        };
        assert_eq!(outcome_exit_code(&failed), 1);
        let completed = HarnessRunOutcome::Completed {
            leaf_id: "l".into(),
            final_entry_id: "e".into(),
            final_message: assistant("ok", StopReason::Stop),
        };
        assert_eq!(outcome_exit_code(&completed), 0);
    }

    #[test]
    fn run_end_outcome_str_roundtrip() {
        assert_eq!(run_end_outcome_str(RunEndOutcome::Completed), "completed");
        assert_eq!(run_end_outcome_str(RunEndOutcome::Aborted), "aborted");
        assert_eq!(run_end_outcome_str(RunEndOutcome::Failed), "failed");
    }
}
