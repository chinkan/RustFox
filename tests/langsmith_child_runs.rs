//! Child LangSmith runs emitted by `AgenticLoop` (no live LangSmith API).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rustfox::langsmith::LangSmithClient;
use rustfox::llm::{
    ChatCompletion, ChatMessage, FunctionCall, LlmClient, ToolCall, ToolDefinition,
};
use rustfox::loop_runner::{AgenticLoop, LoopConfig, LoopOutcome, MessageContainer};
use rustfox::mcp::McpManager;
use rustfox::platform::sender::{MessageFormat, PlatformMessageId, PlatformSender};
use rustfox::provider::{Provider, ProviderConfig, ProviderRegistry};
use rustfox::tool_registry::{ToolContext, ToolHandler, ToolRegistry, ToolUiMode};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CHAIN: &str = "parent-chain";

enum Step {
    Text(String),
    Tools(Vec<ToolCall>),
    Fail(&'static str),
}

struct ScriptLlm {
    config: ProviderConfig,
    steps: Mutex<VecDeque<Step>>,
}

impl ScriptLlm {
    fn client(steps: Vec<Step>) -> LlmClient {
        let provider = Arc::new(Self {
            config: ProviderConfig {
                name: "fixture".into(),
                provider_type: rustfox::config::ProviderType::OpenRouter,
                base_url: "http://fixture.invalid/v1".into(),
                api_key: None,
                default_model: "stub".into(),
                supports_vision: false,
                max_tokens: 64,
                discover_models: false,
                context_window: 4096,
                context_window_cache: Arc::new(tokio::sync::RwLock::new(None)),
                parse_retry_limit: 0,
                rate_limit_retry_limit: 0,
            },
            steps: Mutex::new(VecDeque::from(steps)),
        });
        let mut providers = HashMap::new();
        providers.insert("fixture".to_string(), provider as Arc<dyn Provider>);
        LlmClient::new(Arc::new(ProviderRegistry::new(providers, "fixture".into())))
    }
}

#[async_trait]
impl Provider for ScriptLlm {
    fn name(&self) -> &str {
        &self.config.name
    }
    fn default_model(&self) -> &str {
        &self.config.default_model
    }
    fn supports_vision(&self) -> bool {
        false
    }
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    async fn chat_completion(
        &self,
        _client: &reqwest::Client,
        _messages: &[ChatMessage],
        _tools: &[ToolDefinition],
        model: &str,
        _max_tokens: u32,
    ) -> anyhow::Result<ChatCompletion> {
        let step = self
            .steps
            .lock()
            .expect("steps")
            .pop_front()
            .unwrap_or(Step::Text("no-more-steps".into()));
        let message = match step {
            Step::Fail(msg) => anyhow::bail!(msg),
            Step::Text(text) => ChatMessage {
                role: "assistant".into(),
                content: Some(rustfox::llm::MessageContent::from_text(text)),
                tool_calls: None,
                tool_call_id: None,
            },
            Step::Tools(calls) => ChatMessage {
                role: "assistant".into(),
                content: None,
                tool_calls: Some(calls),
                tool_call_id: None,
            },
        };
        Ok(ChatCompletion {
            message,
            finish_reason: Some("stop".into()),
            model: model.to_string(),
        })
    }

    async fn list_models(&self, _client: &reqwest::Client) -> anyhow::Result<Vec<String>> {
        Ok(vec!["stub".into()])
    }
}

fn tool_call(id: &str, name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: name.into(),
            arguments: arguments.into(),
        },
    }
}

struct MapTools {
    results: HashMap<String, Result<String, String>>,
}

#[async_trait]
impl ToolHandler for MapTools {
    fn define(&self) -> Vec<ToolDefinition> {
        self.results
            .keys()
            .map(|name| ToolDefinition {
                tool_type: "function".into(),
                function: rustfox::llm::FunctionDefinition {
                    name: name.clone(),
                    description: "test tool".into(),
                    parameters: json!({"type": "object", "properties": {}}),
                },
            })
            .collect()
    }

    async fn execute(&self, name: &str, _args: Value, _ctx: ToolContext) -> anyhow::Result<String> {
        match self.results.get(name) {
            Some(Ok(v)) => Ok(v.clone()),
            Some(Err(e)) => anyhow::bail!("{e}"),
            None => anyhow::bail!("unknown {name}"),
        }
    }
}

struct NullSender;

#[async_trait]
impl PlatformSender for NullSender {
    async fn send_message(
        &self,
        _chat_id: &str,
        _text: &str,
        _format: MessageFormat,
    ) -> anyhow::Result<PlatformMessageId> {
        Ok("0".into())
    }
    async fn send_file(
        &self,
        _chat_id: &str,
        _path: &std::path::Path,
        _caption: Option<&str>,
    ) -> anyhow::Result<PlatformMessageId> {
        Ok("0".into())
    }
    async fn show_cancel_button(
        &self,
        _chat_id: &str,
        _text: &str,
        _cancel_id: &str,
    ) -> anyhow::Result<PlatformMessageId> {
        Ok("0".into())
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

fn loop_config(allowed: Option<Vec<String>>, project: Option<String>) -> LoopConfig {
    LoopConfig {
        max_iterations: 4,
        empty_response_retry_limit: 1,
        context_window: 4096,
        loop_detection_enabled: false,
        loop_detection_threshold: 3,
        interactive_loop_callback: false,
        allowed_tools: allowed,
        langsmith_project: project,
        model: None,
        tool_event_tx: None,
        stream_token_tx: None,
        recovery_nudge: None,
        session_key: None,
    }
}

fn registry(results: HashMap<String, Result<String, String>>) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(MapTools { results }));
    tools
}

fn ctx_factory(sender: &Arc<NullSender>) -> impl Fn(&str, &str) -> ToolContext + '_ {
    move |uid: &str, cid: &str| ToolContext {
        sandbox_dir: std::path::PathBuf::from("/tmp"),
        home_dir: None,
        sender: sender.clone(),
        cancel_registry: Arc::new(rustfox::cancel_registry::CancelRegistry::new()),
        user_id: uid.into(),
        chat_id: cid.into(),
        bot_id: "main".into(),
        tool_ui_mode: ToolUiMode::Silent,
    }
}

async fn mount_langsmith(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/runs"))
        .respond_with(ResponseTemplate::new(202))
        .mount(server)
        .await;
    Mock::given(method("PATCH"))
        .and(path_regex(r"^/runs/.+"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

fn enabled_client(server: &MockServer) -> LangSmithClient {
    LangSmithClient::new(Some(&rustfox::config::LangSmithConfig {
        api_key: "test-not-a-secret".into(),
        project: "rustfox".into(),
        base_url: server.uri(),
    }))
}

fn body(req: &Request) -> Value {
    serde_json::from_slice(&req.body).unwrap_or(Value::Null)
}

async fn wait_until(server: &MockServer, pred: impl Fn(&[Request]) -> bool) -> Vec<Request> {
    let start = tokio::time::Instant::now();
    loop {
        let reqs = server.received_requests().await.unwrap_or_default();
        if pred(&reqs) || start.elapsed() > Duration::from_secs(3) {
            return reqs;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn posts(reqs: &[Request]) -> Vec<Value> {
    reqs.iter()
        .filter(|r| r.method == "POST")
        .map(body)
        .collect()
}

fn patch_for<'a>(reqs: &'a [Request], id: &str) -> Option<&'a Request> {
    let suffix = format!("/runs/{id}");
    reqs.iter()
        .find(|r| r.method == "PATCH" && r.url.path().ends_with(&suffix))
}

fn assert_child(post: &Value, name: &str) {
    assert_eq!(post["name"], name, "run name: {post}");
    assert_eq!(post["parent_run_id"], CHAIN, "parent: {post}");
    assert_ne!(
        post["run_type"], "chain",
        "child must not be a parent chain"
    );
    assert_ne!(post["name"], "rustfox_request");
}

fn assert_ended(reqs: &[Request], post: &Value, expect_error: bool) {
    let id = post["id"].as_str().expect("id");
    let patch = patch_for(reqs, id).unwrap_or_else(|| panic!("no end for {id}"));
    let end = body(patch);
    assert!(end.get("end_time").is_some(), "missing end_time: {end}");
    if expect_error {
        assert!(
            end.get("error").and_then(|e| e.as_str()).is_some(),
            "expected error on {name}: {end}",
            name = post["name"]
        );
        assert!(
            end.get("outputs").is_none(),
            "failure should not send outputs"
        );
    } else {
        assert!(end.get("error").is_none(), "unexpected error: {end}");
        assert!(end.get("outputs").is_some(), "missing outputs: {end}");
    }
}

fn user_messages() -> MessageContainer {
    MessageContainer::Plain(vec![ChatMessage {
        role: "user".into(),
        content: Some(rustfox::llm::MessageContent::from_text("hi")),
        tool_calls: None,
        tool_call_id: None,
    }])
}

#[tokio::test]
async fn tool_turn_posts_ordered_child_tool_runs_and_ends_them() {
    let server = MockServer::start().await;
    mount_langsmith(&server).await;
    let client = enabled_client(&server);
    let llm = ScriptLlm::client(vec![
        Step::Tools(vec![
            tool_call("c1", "alpha", r#"{"q":1}"#),
            tool_call("c2", "beta", r#"{"q":2}"#),
        ]),
        Step::Text("done".into()),
    ]);
    let mut results = HashMap::new();
    results.insert("alpha".into(), Ok("ok-alpha".into()));
    results.insert("beta".into(), Ok("ok-beta".into()));
    let tools = registry(results);
    let mcp = McpManager::new();
    let cfg = loop_config(None, Some("rustfox".into()));
    let sender = Arc::new(NullSender);
    let make_ctx = ctx_factory(&sender);
    let mut messages = user_messages();
    let outcome = AgenticLoop::new(
        &llm,
        &tools,
        &mcp,
        &cfg,
        None,
        Some(CHAIN.into()),
        Some(&client),
        sender.as_ref(),
        Box::new(make_ctx),
        None,
    )
    .run(&mut messages, "u", "c")
    .await
    .expect("loop");

    match outcome {
        LoopOutcome::FinalResponse { text, iterations } => {
            assert_eq!(text, "done");
            assert_eq!(iterations, 2, "tool round + answer round");
        }
        other => panic!("unexpected {other:?}"),
    }

    let reqs = wait_until(&server, |r| posts(r).len() >= 4).await;
    let tool_posts: Vec<_> = posts(&reqs)
        .into_iter()
        .filter(|p| p["run_type"] == "tool")
        .collect();
    assert_eq!(tool_posts.len(), 2, "posts: {tool_posts:?}");
    assert_child(&tool_posts[0], "alpha");
    assert_child(&tool_posts[1], "beta");
    assert_eq!(tool_posts[0]["inputs"]["arguments"]["q"], 1);
    assert_eq!(tool_posts[1]["inputs"]["arguments"]["q"], 2);
    assert_eq!(tool_posts[0]["session_name"], "rustfox");
    let end0 = body(patch_for(&reqs, tool_posts[0]["id"].as_str().unwrap()).unwrap());
    let end1 = body(patch_for(&reqs, tool_posts[1]["id"].as_str().unwrap()).unwrap());
    assert_eq!(end0["outputs"]["result"], "ok-alpha");
    assert_eq!(end1["outputs"]["result"], "ok-beta");
    assert_ended(&reqs, &tool_posts[0], false);
    assert_ended(&reqs, &tool_posts[1], false);
}

#[tokio::test]
async fn failed_tool_still_ends_the_child_with_error() {
    let server = MockServer::start().await;
    mount_langsmith(&server).await;
    let client = enabled_client(&server);
    let llm = ScriptLlm::client(vec![
        Step::Tools(vec![tool_call("c1", "boom", "{}")]),
        Step::Text("after-fail".into()),
    ]);
    let mut results = HashMap::new();
    results.insert("boom".into(), Err("boom-failed".into()));
    let tools = registry(results);
    let mcp = McpManager::new();
    let cfg = loop_config(None, Some("rustfox".into()));
    let sender = Arc::new(NullSender);
    let make_ctx = ctx_factory(&sender);
    let mut messages = user_messages();
    let outcome = AgenticLoop::new(
        &llm,
        &tools,
        &mcp,
        &cfg,
        None,
        Some(CHAIN.into()),
        Some(&client),
        sender.as_ref(),
        Box::new(make_ctx),
        None,
    )
    .run(&mut messages, "u", "c")
    .await
    .expect("failed tool must not abort the turn");
    assert!(matches!(
        outcome,
        LoopOutcome::FinalResponse { iterations: 2, .. }
    ));

    let reqs = wait_until(&server, |r| {
        posts(r).iter().any(|p| p["name"] == "boom") && r.iter().any(|req| req.method == "PATCH")
    })
    .await;
    let tool = posts(&reqs)
        .into_iter()
        .find(|p| p["name"] == "boom")
        .expect("boom post");
    assert_child(&tool, "boom");
    let end = body(patch_for(&reqs, tool["id"].as_str().unwrap()).expect("end"));
    assert!(
        end["error"].as_str().unwrap_or("").contains("boom-failed"),
        "{end}"
    );
    assert!(end.get("outputs").is_none());
    assert!(end.get("end_time").is_some());
}

#[tokio::test]
async fn mcp_failure_whitelist_and_hard_invoke_end_the_child() {
    let server = MockServer::start().await;
    mount_langsmith(&server).await;
    let client = enabled_client(&server);
    let llm = ScriptLlm::client(vec![
        Step::Tools(vec![tool_call("m", "mcp_ghost_search", r#"{"q":"x"}"#)]),
        Step::Text("after-mcp".into()),
    ]);
    let tools = registry(HashMap::new());
    let mcp = McpManager::new();
    let cfg = loop_config(None, Some("rustfox".into()));
    let sender = Arc::new(NullSender);
    let make_ctx = ctx_factory(&sender);
    let mut messages = user_messages();
    AgenticLoop::new(
        &llm,
        &tools,
        &mcp,
        &cfg,
        None,
        Some(CHAIN.into()),
        Some(&client),
        sender.as_ref(),
        Box::new(make_ctx),
        None,
    )
    .run(&mut messages, "u", "c")
    .await
    .unwrap();

    let llm = ScriptLlm::client(vec![
        Step::Tools(vec![tool_call("w", "not_allowed", "{}")]),
        Step::Text("after-whitelist".into()),
    ]);
    let cfg = loop_config(Some(vec!["other".into()]), Some("rustfox".into()));
    let sender = Arc::new(NullSender);
    let make_ctx = ctx_factory(&sender);
    let mut messages = user_messages();
    let tools = registry(HashMap::new());
    let mcp = McpManager::new();
    AgenticLoop::new(
        &llm,
        &tools,
        &mcp,
        &cfg,
        None,
        Some(CHAIN.into()),
        Some(&client),
        sender.as_ref(),
        Box::new(make_ctx),
        None,
    )
    .run(&mut messages, "u", "c")
    .await
    .unwrap();

    let llm = ScriptLlm::client(vec![Step::Tools(vec![tool_call(
        "h",
        "invoke_agent",
        r#"{"agent":"self"}"#,
    )])]);
    let cfg = loop_config(None, Some("rustfox".into()));
    let sender = Arc::new(NullSender);
    let make_ctx = ctx_factory(&sender);
    let tools = registry(HashMap::new());
    let mcp = McpManager::new();
    let handler: rustfox::loop_runner::ToolHandlerFn = Box::new(|name, _args, _u, _c| {
        let name = name.to_string();
        Box::pin(async move {
            if name == "invoke_agent" {
                Some(
                    "Peer invoke cycle detected: 'self' is already on the invoke stack [main]"
                        .into(),
                )
            } else {
                None
            }
        })
    });
    let err = AgenticLoop::new(
        &llm,
        &tools,
        &mcp,
        &cfg,
        None,
        Some(CHAIN.into()),
        Some(&client),
        sender.as_ref(),
        Box::new(make_ctx),
        Some(handler),
    )
    .run(&mut user_messages(), "u", "c")
    .await
    .expect_err("hard invoke aborts");
    assert!(err.to_string().contains("Peer invoke cycle"));

    let reqs = wait_until(&server, |r| {
        let names: Vec<_> = posts(r)
            .iter()
            .filter(|p| p["run_type"] == "tool")
            .map(|p| p["name"].as_str().unwrap_or("").to_string())
            .collect();
        names.contains(&"mcp_ghost_search".into())
            && names.contains(&"not_allowed".into())
            && names.contains(&"invoke_agent".into())
            && r.iter().filter(|req| req.method == "PATCH").count() >= 3
    })
    .await;
    for name in ["mcp_ghost_search", "not_allowed", "invoke_agent"] {
        let post = posts(&reqs)
            .into_iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_child(&post, name);
        assert_ended(&reqs, &post, true);
    }
    let mcp_end = body(
        patch_for(
            &reqs,
            posts(&reqs)
                .iter()
                .find(|p| p["name"] == "mcp_ghost_search")
                .unwrap()["id"]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
    );
    assert!(mcp_end["error"]
        .as_str()
        .unwrap()
        .contains("MCP tool not found"));
    let white_end = body(
        patch_for(
            &reqs,
            posts(&reqs)
                .iter()
                .find(|p| p["name"] == "not_allowed")
                .unwrap()["id"]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
    );
    assert!(white_end["error"]
        .as_str()
        .unwrap()
        .contains("not available"));
}

#[tokio::test]
async fn llm_call_is_a_child_and_is_ended_on_success_and_failure() {
    let server = MockServer::start().await;
    mount_langsmith(&server).await;
    let client = enabled_client(&server);

    let llm = ScriptLlm::client(vec![Step::Text("one-shot".into())]);
    let tools = registry(HashMap::new());
    let mcp = McpManager::new();
    let cfg = loop_config(None, Some("rustfox".into()));
    let sender = Arc::new(NullSender);
    let make_ctx = ctx_factory(&sender);
    let outcome = AgenticLoop::new(
        &llm,
        &tools,
        &mcp,
        &cfg,
        None,
        Some(CHAIN.into()),
        Some(&client),
        sender.as_ref(),
        Box::new(make_ctx),
        None,
    )
    .run(&mut user_messages(), "u", "c")
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        LoopOutcome::FinalResponse { iterations: 1, .. }
    ));

    let llm = ScriptLlm::client(vec![Step::Fail("upstream down")]);
    let tools = registry(HashMap::new());
    let mcp = McpManager::new();
    let cfg = loop_config(None, Some("rustfox".into()));
    let sender = Arc::new(NullSender);
    let make_ctx = ctx_factory(&sender);
    let err = AgenticLoop::new(
        &llm,
        &tools,
        &mcp,
        &cfg,
        None,
        Some(CHAIN.into()),
        Some(&client),
        sender.as_ref(),
        Box::new(make_ctx),
        None,
    )
    .run(&mut user_messages(), "u", "c")
    .await
    .expect_err("llm failure");
    assert!(err.to_string().contains("upstream down"));

    let reqs = wait_until(&server, |r| {
        posts(r).iter().filter(|p| p["name"] == "llm_call").count() >= 2
            && r.iter().filter(|req| req.method == "PATCH").count() >= 2
    })
    .await;
    let llm_posts: Vec<_> = posts(&reqs)
        .into_iter()
        .filter(|p| p["name"] == "llm_call")
        .collect();
    assert!(llm_posts.len() >= 2, "{llm_posts:?}");
    for post in &llm_posts {
        assert_eq!(post["run_type"], "llm");
        assert_child(post, "llm_call");
    }
    let mut saw_ok = false;
    let mut saw_err = false;
    for post in &llm_posts {
        let end = body(patch_for(&reqs, post["id"].as_str().unwrap()).expect("llm end"));
        assert!(end.get("end_time").is_some());
        if end.get("error").is_some() {
            assert!(end["error"].as_str().unwrap().contains("upstream down"));
            saw_err = true;
        } else {
            assert_eq!(end["outputs"]["content"], "one-shot");
            saw_ok = true;
        }
    }
    assert!(saw_ok && saw_err, "both llm endings: {reqs:?}");
    assert!(posts(&reqs).iter().all(|p| p["run_type"] != "chain"));
}

#[tokio::test]
async fn subagent_loop_reuses_chain_and_does_not_open_a_second_parent() {
    let server = MockServer::start().await;
    mount_langsmith(&server).await;
    let client = Arc::new(enabled_client(&server));
    let outer_llm = ScriptLlm::client(vec![
        Step::Tools(vec![
            tool_call("s", "spawn_agents", r#"{"prompt":"p"}"#),
            tool_call("i", "invoke_agent", r#"{"agent":"peer","prompt":"go"}"#),
        ]),
        Step::Text("outer-done".into()),
    ]);
    let inner_llm = ScriptLlm::client(vec![
        Step::Tools(vec![tool_call("n", "inner_tool", r#"{"n":1}"#)]),
        Step::Text("inner-done".into()),
    ]);
    let tools = registry(HashMap::new());
    let mcp = McpManager::new();
    let cfg = loop_config(None, Some("rustfox".into()));
    let sender = Arc::new(NullSender);
    let make_ctx = ctx_factory(&sender);

    let nested_llm = inner_llm.clone();
    let nested_client = Arc::clone(&client);
    let handler: rustfox::loop_runner::ToolHandlerFn = Box::new(move |name, _args, _u, _c| {
        let name = name.to_string();
        let nested_llm = nested_llm.clone();
        let nested_client = Arc::clone(&nested_client);
        Box::pin(async move {
            match name.as_str() {
                "spawn_agents" => Some("spawned".into()),
                "invoke_agent" => {
                    let mut inner_results = HashMap::new();
                    inner_results.insert("inner_tool".into(), Ok("inner-ok".into()));
                    let inner_tools = registry(inner_results);
                    let inner_mcp = McpManager::new();
                    let inner_cfg = loop_config(None, Some("rustfox".into()));
                    let inner_sender = Arc::new(NullSender);
                    let inner_ctx = ctx_factory(&inner_sender);
                    let outcome = AgenticLoop::new(
                        &nested_llm,
                        &inner_tools,
                        &inner_mcp,
                        &inner_cfg,
                        None,
                        Some(CHAIN.into()),
                        Some(nested_client.as_ref()),
                        inner_sender.as_ref(),
                        Box::new(inner_ctx),
                        None,
                    )
                    .run(&mut user_messages(), "u", "c")
                    .await
                    .expect("inner");
                    let LoopOutcome::FinalResponse { text, iterations } = outcome else {
                        panic!("inner outcome");
                    };
                    assert_eq!(iterations, 2);
                    Some(text)
                }
                _ => None,
            }
        })
    });

    let outcome = AgenticLoop::new(
        &outer_llm,
        &tools,
        &mcp,
        &cfg,
        None,
        Some(CHAIN.into()),
        Some(client.as_ref()),
        sender.as_ref(),
        Box::new(make_ctx),
        Some(handler),
    )
    .run(&mut user_messages(), "u", "c")
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        LoopOutcome::FinalResponse { iterations: 2, .. }
    ));

    let reqs = wait_until(&server, |r| {
        let names: Vec<_> = posts(r)
            .iter()
            .map(|p| p["name"].as_str().unwrap_or("").to_string())
            .collect();
        names.iter().filter(|n| *n == "llm_call").count() >= 3
            && names.contains(&"spawn_agents".to_string())
            && names.contains(&"invoke_agent".to_string())
            && names.contains(&"inner_tool".to_string())
    })
    .await;
    let posted = posts(&reqs);
    assert!(
        posted.iter().all(|p| p["parent_run_id"] == CHAIN),
        "every child hangs on the parent chain: {posted:?}"
    );
    assert!(
        posted.iter().all(|p| p["run_type"] != "chain"),
        "subagent must not open a second parent: {posted:?}"
    );
    assert!(posted.iter().all(|p| p["name"] != "rustfox_request"));
    for name in ["spawn_agents", "invoke_agent", "inner_tool", "llm_call"] {
        let post = posted.iter().find(|p| p["name"] == name).unwrap();
        let expect_error = false;
        assert_ended(&reqs, post, expect_error);
    }
    let inner = posted.iter().find(|p| p["name"] == "inner_tool").unwrap();
    let end = body(patch_for(&reqs, inner["id"].as_str().unwrap()).unwrap());
    assert_eq!(end["outputs"]["result"], "inner-ok");
}

#[tokio::test]
async fn one_shot_iteration_count_is_one() {
    let llm = ScriptLlm::client(vec![Step::Text("hi-back".into())]);
    let tools = registry(HashMap::new());
    let mcp = McpManager::new();
    let cfg = loop_config(None, None);
    let sender = Arc::new(NullSender);
    let make_ctx = ctx_factory(&sender);
    let client = LangSmithClient::new(None);
    let outcome = AgenticLoop::new(
        &llm,
        &tools,
        &mcp,
        &cfg,
        None,
        Some(CHAIN.into()),
        Some(&client),
        sender.as_ref(),
        Box::new(make_ctx),
        None,
    )
    .run(&mut user_messages(), "u", "c")
    .await
    .unwrap();
    match outcome {
        LoopOutcome::FinalResponse { text, iterations } => {
            assert_eq!(text, "hi-back");
            assert_eq!(iterations, 1);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn tracing_disabled_makes_no_http_call_and_does_not_panic() {
    let server = MockServer::start().await;
    mount_langsmith(&server).await;
    let client = LangSmithClient::new(None);
    assert!(!client.is_enabled());
    client.start_run(rustfox::langsmith::RunParams {
        id: "should-not-send".into(),
        name: "llm_call".into(),
        run_type: rustfox::langsmith::RunType::Llm,
        parent_run_id: Some(CHAIN.into()),
        inputs: json!({}),
        session_name: "rustfox".into(),
        start_time: "2026-01-01T00:00:00.000Z".into(),
    });
    client.end_run(rustfox::langsmith::EndRunParams {
        id: "should-not-send".into(),
        outputs: Some(json!({"result": "x"})),
        error: None,
        end_time: "2026-01-01T00:00:00.000Z".into(),
    });

    let llm = ScriptLlm::client(vec![
        Step::Tools(vec![tool_call("c", "alpha", "{}")]),
        Step::Text("ok".into()),
    ]);
    let mut results = HashMap::new();
    results.insert("alpha".into(), Ok("a".into()));
    let tools = registry(results);
    let mcp = McpManager::new();
    let cfg = loop_config(None, Some(server.uri()));
    let sender = Arc::new(NullSender);
    let make_ctx = ctx_factory(&sender);
    let outcome = AgenticLoop::new(
        &llm,
        &tools,
        &mcp,
        &cfg,
        None,
        Some(CHAIN.into()),
        Some(&client),
        sender.as_ref(),
        Box::new(make_ctx),
        None,
    )
    .run(&mut user_messages(), "u", "c")
    .await
    .expect("disabled tracing must not panic");
    assert!(matches!(
        outcome,
        LoopOutcome::FinalResponse { iterations: 2, .. }
    ));

    tokio::time::sleep(Duration::from_millis(200)).await;
    let reqs = server.received_requests().await.unwrap_or_default();
    assert!(
        reqs.is_empty(),
        "disabled client must not call LangSmith, saw {} requests",
        reqs.len()
    );
}
