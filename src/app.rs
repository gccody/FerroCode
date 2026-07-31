use crate::{
    backend::CodexBackend,
    child_process::hidden_command,
    model::{
        AccountInfo, AppHistory, Approval, ApprovalChoice, ContextWindowUsage, ConversationItem,
        ItemKind, LocalThread, ModelOption, PlanUsage, Preferences, Project, RateLimitWindow,
        ResetCredit, SandboxChoice,
    },
    theme,
};
use crossbeam_channel::{Receiver, unbounded};
use eframe::{App, CreationContext, Frame, Storage, egui};
use egui::{Align, Color32, CornerRadius, FontId, Layout, RichText, ScrollArea, Stroke, TextEdit};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    thread,
};
use windows_sys::Win32::{
    Foundation::{FILETIME, SYSTEMTIME},
    System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
};

const PREFS_KEY: &str = "codeagent.preferences";
const HISTORY_KEY: &str = "codeagent.history.v1";

#[derive(Debug, Clone)]
enum PendingCall {
    Initialize,
    Account,
    RateLimits,
    ConsumeReset,
    Models,
    ThreadStart {
        local_thread_id: String,
        turn: PendingTurn,
    },
    TurnStart {
        local_thread_id: String,
    },
    Interrupt {
        local_thread_id: String,
    },
    SummaryThreadStart(SummaryJob),
    SummaryTurnStart {
        thread_id: String,
    },
}

#[derive(Debug, Clone)]
struct PendingTurn {
    input: Vec<Value>,
    cwd: String,
    approval_policy: String,
    sandbox: String,
    model: String,
    effort: String,
}

#[derive(Debug, Clone)]
enum SummaryTarget {
    ThreadTitle {
        local_thread_id: String,
    },
    ActivityGroup {
        local_thread_id: String,
        first_item_id: String,
    },
}

#[derive(Debug, Clone)]
struct SummaryJob {
    key: String,
    target: SummaryTarget,
    prompt: String,
}

#[derive(Debug, Clone)]
struct ActiveSummary {
    turn_id: Option<String>,
    target: SummaryTarget,
    output: String,
}

#[derive(Clone)]
struct UserQuestion {
    id: String,
    header: String,
    question: String,
    options: Vec<(String, String)>,
    answer: String,
    secret: bool,
}

#[derive(Clone)]
struct UserQuestionRequest {
    request_id: Value,
    local_thread_id: Option<String>,
    questions: Vec<UserQuestion>,
}

#[derive(Clone)]
struct ResetConfirmation {
    credit_id: Option<String>,
    title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsCategory {
    General,
    Agent,
    Summaries,
    Interface,
    Codex,
}

impl SettingsCategory {
    const ALL: [Self; 5] = [
        Self::General,
        Self::Agent,
        Self::Summaries,
        Self::Interface,
        Self::Codex,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Agent => "Agent",
            Self::Summaries => "Summaries",
            Self::Interface => "Interface",
            Self::Codex => "Codex",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::General => "Manage the active project used by CodeAgent.",
            Self::Agent => "Choose the default permissions for new turns.",
            Self::Summaries => "Configure how conversation summaries are generated.",
            Self::Interface => "Control which workspace tools are visible.",
            Self::Codex => "View your Codex account, plan usage, and available resets.",
        }
    }
}

impl ContextWindowUsage {
    fn from_notification(params: &Value) -> Option<Self> {
        let used_tokens = params
            .pointer("/tokenUsage/last/totalTokens")
            .and_then(Value::as_u64)?;
        let capacity_tokens = params
            .pointer("/tokenUsage/modelContextWindow")
            .and_then(Value::as_u64)?;
        (capacity_tokens > 0).then_some(Self {
            used_tokens,
            capacity_tokens,
        })
    }
}

pub struct CodeAgentApp {
    backend: Option<CodexBackend>,
    backend_start_rx: Option<Receiver<Result<CodexBackend, String>>>,
    deferred_history: Option<String>,
    history_rx: Option<Receiver<AppHistory>>,
    history_loaded: bool,
    startup_deferred: bool,
    pending: HashMap<u64, PendingCall>,
    next_id: u64,
    connected: bool,
    connection_text: String,
    account: AccountInfo,
    plan_usage: Option<PlanUsage>,
    usage_loading: bool,
    usage_error: Option<String>,
    reset_confirmation: Option<ResetConfirmation>,
    reset_in_progress: bool,
    models: Vec<ModelOption>,
    projects: Vec<Project>,
    threads: Vec<LocalThread>,
    active_project: Option<String>,
    active_local_thread: Option<String>,
    runtime_threads: HashMap<String, String>,
    active_thread: Option<String>,
    active_turn: Option<String>,
    running_turns: HashMap<String, Option<String>>,
    conversation: Vec<ConversationItem>,
    prompt: String,
    attachments: Vec<String>,
    search: String,
    prefs: Preferences,
    approval: Option<Approval>,
    approval_queue: VecDeque<Approval>,
    user_question: Option<UserQuestionRequest>,
    user_question_queue: VecDeque<UserQuestionRequest>,
    toast: Option<(String, f64, bool)>,
    inspector_tab: usize,
    activity_log: Vec<String>,
    git_diff: String,
    files: Vec<String>,
    workspace_rx: Option<Receiver<(String, Vec<String>)>>,
    should_scroll: bool,
    show_settings: bool,
    settings_category: SettingsCategory,
    summary_queue: VecDeque<SummaryJob>,
    summary_pending_keys: HashSet<String>,
    active_summaries: HashMap<String, ActiveSummary>,
    markdown_cache: CommonMarkCache,
}

impl CodeAgentApp {
    pub fn new(cc: &CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let prefs: Preferences = cc
            .storage
            .and_then(|s| eframe::get_value(s, PREFS_KEY))
            .unwrap_or_default();
        // Copy the persisted RON now, but parse potentially large conversation history only
        // after the first frame has been presented.
        let deferred_history = cc.storage.and_then(|s| s.get_string(HISTORY_KEY));
        let history_loaded = deferred_history.is_none();
        Self {
            backend: None,
            backend_start_rx: None,
            deferred_history,
            history_rx: None,
            history_loaded,
            startup_deferred: true,
            pending: HashMap::new(),
            next_id: 1,
            connected: false,
            connection_text: "Starting Codex…".into(),
            account: AccountInfo::default(),
            plan_usage: None,
            usage_loading: false,
            usage_error: None,
            reset_confirmation: None,
            reset_in_progress: false,
            models: Vec::new(),
            projects: Vec::new(),
            threads: Vec::new(),
            active_project: None,
            active_local_thread: None,
            runtime_threads: HashMap::new(),
            active_thread: None,
            active_turn: None,
            running_turns: HashMap::new(),
            conversation: Vec::new(),
            prompt: String::new(),
            attachments: Vec::new(),
            search: String::new(),
            prefs,
            approval: None,
            approval_queue: VecDeque::new(),
            user_question: None,
            user_question_queue: VecDeque::new(),
            toast: None,
            inspector_tab: 0,
            activity_log: vec!["CodeAgent launched".into()],
            git_diff: String::new(),
            files: Vec::new(),
            workspace_rx: None,
            should_scroll: false,
            show_settings: false,
            settings_category: SettingsCategory::General,
            summary_queue: VecDeque::new(),
            summary_pending_keys: HashSet::new(),
            active_summaries: HashMap::new(),
            markdown_cache: CommonMarkCache::default(),
        }
    }

    fn start_backend(&mut self, ctx: &egui::Context) {
        if self.backend_start_rx.is_some() {
            return;
        }
        self.backend = None;
        self.pending.clear();
        self.connected = false;
        self.connection_text = "Starting Codexâ€¦".into();
        let (tx, rx) = unbounded();
        self.backend_start_rx = Some(rx);
        let repaint = ctx.clone();
        thread::Builder::new()
            .name("codex-startup".into())
            .spawn(move || {
                let _ = tx.send(CodexBackend::spawn());
                repaint.request_repaint();
            })
            .expect("spawn Codex startup thread");
    }

    fn attach_backend(&mut self, result: Result<CodexBackend, String>) {
        match result {
            Ok(backend) => {
                self.backend = Some(backend);
                self.connection_text = "Connecting…".into();
                self.request(
                    "initialize",
                    json!({
                        "clientInfo": {"name":"codeagent","title":"CodeAgent","version":env!("CARGO_PKG_VERSION")},
                        "capabilities": {"experimentalApi":true,"requestAttestation":false}
                    }),
                    PendingCall::Initialize,
                );
            }
            Err(err) => {
                self.connection_text = "Codex unavailable".into();
                self.error(err);
            }
        }
    }

    fn begin_deferred_startup(&mut self, ctx: &egui::Context) {
        self.start_backend(ctx);
        if let Some(encoded) = self.deferred_history.take() {
            let (tx, rx) = unbounded();
            self.history_rx = Some(rx);
            let repaint = ctx.clone();
            thread::Builder::new()
                .name("history-loader".into())
                .spawn(move || {
                    let history = ron::from_str(&encoded).unwrap_or_default();
                    let _ = tx.send(history);
                    repaint.request_repaint();
                })
                .expect("spawn history loader thread");
        }
    }

    fn process_startup(&mut self) {
        if let Some(result) = self
            .backend_start_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.backend_start_rx = None;
            self.attach_backend(result);
        }
        if let Some(history) = self.history_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.history_rx = None;
            self.apply_history(history);
            self.history_loaded = true;
            self.refresh_workspace_info();
        }
    }

    fn apply_history(&mut self, mut history: AppHistory) {
        for thread in &mut history.threads {
            for item in &mut thread.messages {
                if item.status != "running"
                    && matches!(
                        item.kind,
                        ItemKind::Command | ItemKind::Tool | ItemKind::FileChange | ItemKind::Plan
                    )
                {
                    item.collapsed = true;
                }
            }
        }
        let active_project = history
            .active_project
            .filter(|id| history.projects.iter().any(|project| &project.id == id));
        let active_local_thread = history.active_thread.filter(|id| {
            history.threads.iter().any(|thread| {
                &thread.id == id && active_project.as_deref() == Some(&thread.project_id)
            })
        });
        self.conversation = active_local_thread
            .as_ref()
            .and_then(|id| history.threads.iter().find(|thread| &thread.id == id))
            .map(|thread| thread.messages.clone())
            .unwrap_or_default();
        if let Some(project) = active_project
            .as_ref()
            .and_then(|id| history.projects.iter().find(|project| &project.id == id))
        {
            self.prefs.workspace.clone_from(&project.path);
        }
        self.projects = history.projects;
        self.threads = history.threads;
        self.active_project = active_project;
        self.active_local_thread = active_local_thread;
    }

    fn request(&mut self, method: &str, params: Value, kind: PendingCall) {
        let id = self.next_id;
        self.next_id += 1;
        if let Some(backend) = &self.backend {
            match backend.send(json!({"method":method,"id":id,"params":params})) {
                Ok(()) => {
                    self.pending.insert(id, kind);
                }
                Err(err) => self.error(err),
            }
        }
    }

    fn notify(&mut self, method: &str, params: Option<Value>) {
        if let Some(backend) = &self.backend {
            let mut message = json!({"method":method});
            if let Some(params) = params {
                message["params"] = params;
            }
            if let Err(err) = backend.send(message) {
                self.error(err);
            }
        }
    }

    fn process_backend(&mut self, ctx: &egui::Context) {
        let messages: Vec<Value> = self
            .backend
            .as_ref()
            .map(|backend| {
                std::iter::from_fn(|| backend.try_recv())
                    .take(300)
                    .collect()
            })
            .unwrap_or_default();
        if !messages.is_empty() {
            ctx.request_repaint();
        }
        for message in messages {
            self.handle_message(message);
        }
        if let Some(rx) = &self.workspace_rx
            && let Ok((diff, files)) = rx.try_recv()
        {
            self.git_diff = diff;
            self.files = files;
            self.workspace_rx = None;
        }
        if self.connected {
            self.queue_missing_summaries();
            self.start_queued_summaries();
        }
    }

    fn handle_message(&mut self, message: Value) {
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            if message.get("method").is_some() {
                self.handle_server_request(message);
                return;
            }
            let kind = self.pending.remove(&id);
            if let Some(error) = message.get("error") {
                let text = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown Codex error");
                match kind {
                    Some(PendingCall::RateLimits) => {
                        self.usage_loading = false;
                        self.usage_error = Some(text.to_owned());
                        return;
                    }
                    Some(PendingCall::ConsumeReset) => {
                        self.reset_in_progress = false;
                        self.usage_error = Some(format!("Could not use reset: {text}"));
                        self.error(format!("Could not use reset: {text}"));
                        return;
                    }
                    Some(PendingCall::SummaryThreadStart(job)) => {
                        self.activity_log
                            .push(format!("Summary unavailable for {}: {text}", job.key));
                        return;
                    }
                    Some(PendingCall::SummaryTurnStart { thread_id }) => {
                        self.active_summaries.remove(&thread_id);
                        self.activity_log
                            .push(format!("Summary unavailable: {text}"));
                        return;
                    }
                    Some(PendingCall::ThreadStart {
                        local_thread_id, ..
                    })
                    | Some(PendingCall::TurnStart { local_thread_id }) => {
                        self.running_turns.remove(&local_thread_id);
                    }
                    _ => {}
                }
                self.error(text.to_owned());
                return;
            }
            if let (Some(kind), Some(result)) = (kind, message.get("result")) {
                self.handle_response(kind, result.clone());
            }
            return;
        }
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            self.handle_notification(method, params);
        }
    }

    fn handle_response(&mut self, kind: PendingCall, result: Value) {
        match kind {
            PendingCall::Initialize => {
                self.connected = true;
                self.connection_text = "Codex connected".into();
                self.activity_log.push(format!(
                    "Connected to {}",
                    result
                        .get("userAgent")
                        .and_then(Value::as_str)
                        .unwrap_or("Codex")
                ));
                self.notify("initialized", None);
                self.request(
                    "account/read",
                    json!({"refreshToken":false}),
                    PendingCall::Account,
                );
                self.request("model/list", json!({"limit":100}), PendingCall::Models);
            }
            PendingCall::Account => {
                self.apply_account(&result);
                if self.account.authenticated {
                    self.refresh_plan_usage();
                }
            }
            PendingCall::RateLimits => {
                self.usage_loading = false;
                self.usage_error = None;
                let usage = PlanUsage::from_protocol(&result);
                if let Some(plan) = usage
                    .limits
                    .iter()
                    .find_map(|limit| (!limit.plan.is_empty()).then_some(limit.plan.as_str()))
                {
                    self.account.plan = plan.to_owned();
                }
                self.plan_usage = Some(usage);
            }
            PendingCall::ConsumeReset => {
                self.reset_in_progress = false;
                let outcome = result
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                match outcome {
                    "reset" => self.info("Codex plan usage was reset".into()),
                    "alreadyRedeemed" => {
                        self.info("This reset was already applied".into());
                    }
                    "nothingToReset" => {
                        self.error("None of your current usage windows can be reset yet".into());
                    }
                    "noCredit" => self.error("No usage resets are available".into()),
                    _ => self.error(format!("Codex returned an unknown reset result: {outcome}")),
                }
                self.refresh_plan_usage();
            }
            PendingCall::Models => self.apply_models(&result),
            PendingCall::ThreadStart {
                local_thread_id,
                turn,
            } => {
                if let Some(thread) = result.get("thread") {
                    let codex_id = thread.get("id").and_then(Value::as_str).map(str::to_owned);
                    if let Some(codex_id) = codex_id {
                        self.runtime_threads
                            .insert(local_thread_id.clone(), codex_id.clone());
                        if self.active_local_thread.as_deref() == Some(&local_thread_id) {
                            self.active_thread = Some(codex_id.clone());
                        }
                        self.start_turn(local_thread_id, codex_id, turn);
                    } else {
                        self.running_turns.remove(&local_thread_id);
                        self.error("Codex started a thread without returning its id".into());
                    }
                } else {
                    self.running_turns.remove(&local_thread_id);
                    self.error("Codex started a thread without returning its id".into());
                }
            }
            PendingCall::TurnStart { local_thread_id } => {
                let turn_id = result
                    .get("turn")
                    .and_then(|v| v.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.running_turns
                    .insert(local_thread_id.clone(), turn_id.clone());
                if self.active_local_thread.as_deref() == Some(&local_thread_id) {
                    self.active_turn = turn_id;
                }
            }
            PendingCall::Interrupt { local_thread_id } => {
                self.running_turns.remove(&local_thread_id);
                if self.active_local_thread.as_deref() == Some(&local_thread_id) {
                    self.active_turn = None;
                }
                self.activity_log.push("Turn interrupted".into());
            }
            PendingCall::SummaryThreadStart(job) => {
                let Some(thread_id) = result
                    .get("thread")
                    .and_then(|thread| thread.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    self.activity_log
                        .push("Summary thread did not return an id".into());
                    return;
                };
                self.activity_log.push(format!(
                    "Summarizing {} with {} ({})",
                    job.key, self.prefs.summary_model, self.prefs.summary_effort
                ));
                self.active_summaries.insert(
                    thread_id.clone(),
                    ActiveSummary {
                        turn_id: None,
                        target: job.target,
                        output: String::new(),
                    },
                );
                self.request(
                    "turn/start",
                    json!({
                        "threadId":thread_id,
                        "input":[{"type":"text","text":job.prompt,"text_elements":[]}],
                        "cwd":self.prefs.workspace,
                        "approvalPolicy":"never",
                        "sandbox":"read-only",
                        "model":self.prefs.summary_model,
                        "effort":self.prefs.summary_effort
                    }),
                    PendingCall::SummaryTurnStart { thread_id },
                );
            }
            PendingCall::SummaryTurnStart { thread_id } => {
                if let Some(active) = self.active_summaries.get_mut(&thread_id) {
                    active.turn_id = result
                        .get("turn")
                        .and_then(|turn| turn.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
            }
        }
    }

    fn handle_notification(&mut self, method: &str, params: Value) {
        if self.handle_summary_notification(method, &params) {
            return;
        }
        match method {
            "backend/exited" | "backend/protocolError" => {
                self.connected = false;
                self.running_turns.clear();
                self.active_turn = None;
                self.connection_text = "Codex disconnected".into();
                self.error(
                    params
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or(method)
                        .to_owned(),
                );
            }
            "backend/stderr" => {
                if let Some(message) = params.get("message").and_then(Value::as_str)
                    && (message.contains("ERROR") || message.contains("WARN"))
                {
                    self.activity_log.push(format!("Codex: {message}"));
                }
            }
            "item/started" => {
                let local_thread_id = self.local_thread_id_for_notification(&params);
                if let (Some(local_thread_id), Some(item)) = (local_thread_id, params.get("item")) {
                    self.ingest_item(&local_thread_id, item, false);
                    if self.active_local_thread.as_deref() == Some(&local_thread_id) {
                        self.should_scroll = true;
                    }
                }
            }
            "item/completed" => {
                let local_thread_id = self.local_thread_id_for_notification(&params);
                if let (Some(local_thread_id), Some(item)) = (local_thread_id, params.get("item")) {
                    self.ingest_item(&local_thread_id, item, true);
                    if self.active_local_thread.as_deref() == Some(&local_thread_id) {
                        self.should_scroll = true;
                    }
                }
            }
            "item/agentMessage/delta" => {
                if let Some(local_thread_id) = self.local_thread_id_for_notification(&params) {
                    self.append_delta(&local_thread_id, &params, ItemKind::Assistant, "Codex");
                }
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                if let Some(local_thread_id) = self.local_thread_id_for_notification(&params) {
                    self.append_delta(&local_thread_id, &params, ItemKind::Reasoning, "Reasoning");
                }
            }
            "item/plan/delta" => {
                if let Some(local_thread_id) = self.local_thread_id_for_notification(&params) {
                    self.append_delta(&local_thread_id, &params, ItemKind::Plan, "Plan");
                }
            }
            "item/commandExecution/outputDelta" => {
                if let Some(local_thread_id) = self.local_thread_id_for_notification(&params) {
                    self.append_delta(&local_thread_id, &params, ItemKind::Command, "Command");
                }
            }
            "turn/started" => {
                let local_thread_id = self.local_thread_id_for_notification(&params);
                let turn_id = params
                    .get("turn")
                    .and_then(|v| v.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(local_thread_id) = local_thread_id {
                    self.running_turns
                        .insert(local_thread_id.clone(), turn_id.clone());
                    if self.active_local_thread.as_deref() == Some(&local_thread_id) {
                        self.active_turn = turn_id;
                    }
                    self.activity_log.push("Agent turn started".into());
                }
            }
            "turn/completed" => {
                let local_thread_id = self.local_thread_id_for_notification(&params);
                if let Some(local_thread_id) = &local_thread_id {
                    self.running_turns.remove(local_thread_id);
                    if self.active_local_thread.as_deref() == Some(local_thread_id) {
                        self.active_turn = None;
                    }
                }
                let status = params
                    .get("turn")
                    .and_then(|v| v.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("completed");
                self.activity_log.push(format!("Turn {status}"));
                if status == "failed" {
                    let message = params
                        .pointer("/turn/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("The Codex turn failed");
                    self.error(message.to_owned());
                }
                if local_thread_id.as_deref() == self.active_local_thread.as_deref() {
                    self.sync_active_conversation();
                    self.refresh_workspace_info();
                    self.should_scroll = true;
                }
            }
            "thread/name/updated" => {}
            "thread/tokenUsage/updated" => {
                if let (Some(local_thread_id), Some(usage)) = (
                    self.local_thread_id_for_notification(&params),
                    ContextWindowUsage::from_notification(&params),
                ) {
                    if let Some(thread) = self
                        .threads
                        .iter_mut()
                        .find(|thread| thread.id == local_thread_id)
                    {
                        thread.context_usage = Some(usage);
                    }
                    self.activity_log.push(format!(
                        "Context: {} of {} tokens",
                        usage.used_tokens, usage.capacity_tokens
                    ));
                }
            }
            "account/updated" => {
                if let Some(account) = params.get("account") {
                    self.apply_account(&json!({"account":account,"requiresOpenaiAuth":true}));
                    if self.account.authenticated {
                        self.refresh_plan_usage();
                    }
                }
            }
            "account/rateLimits/updated" => self.refresh_plan_usage(),
            "warning" | "configWarning" | "error" => {
                let msg = params
                    .get("message")
                    .or_else(|| params.get("summary"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex reported a warning");
                if !is_skills_budget_warning(msg) {
                    self.error(msg.to_owned());
                }
            }
            _ => {}
        }
    }

    fn local_thread_id_for_notification(&self, params: &Value) -> Option<String> {
        let runtime_id = params.get("threadId").and_then(Value::as_str)?;
        self.runtime_threads
            .iter()
            .find_map(|(local_id, codex_id)| (codex_id == runtime_id).then(|| local_id.clone()))
    }

    fn handle_server_request(&mut self, message: Value) {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let request_id = message.get("id").cloned().unwrap_or(Value::Null);
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match method.as_str() {
            "item/commandExecution/requestApproval" | "execCommandApproval" => {
                let command = params
                    .get("command")
                    .map(value_to_text)
                    .unwrap_or_else(|| "Command requested".into());
                let cwd = params
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let reason = params
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex needs permission to run this command.");
                self.enqueue_approval(Approval {
                    request_id,
                    method,
                    title: "Run command?".into(),
                    detail: format!("{command}\n\nWorking directory: {cwd}\n{reason}"),
                    allow_session: true,
                });
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => {
                let reason = params
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex wants to edit files in this workspace.");
                let root = params
                    .get("grantRoot")
                    .and_then(Value::as_str)
                    .unwrap_or(&self.prefs.workspace);
                self.enqueue_approval(Approval {
                    request_id,
                    method,
                    title: "Apply file changes?".into(),
                    detail: format!("{reason}\n\nTarget: {root}"),
                    allow_session: true,
                });
            }
            "item/tool/requestUserInput" => {
                let local_thread_id = self.local_thread_id_for_notification(&params);
                let questions = params
                    .get("questions")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|question| {
                        let id = question.get("id")?.as_str()?.to_owned();
                        let options = question
                            .get("options")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(|option| {
                                Some((
                                    option.get("label")?.as_str()?.to_owned(),
                                    option
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_owned(),
                                ))
                            })
                            .collect::<Vec<_>>();
                        Some(UserQuestion {
                            id,
                            header: question
                                .get("header")
                                .and_then(Value::as_str)
                                .unwrap_or("Question")
                                .to_owned(),
                            question: question
                                .get("question")
                                .and_then(Value::as_str)
                                .unwrap_or("Codex needs input")
                                .to_owned(),
                            answer: options.first().map(|o| o.0.clone()).unwrap_or_default(),
                            options,
                            secret: question
                                .get("isSecret")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .collect();
                self.enqueue_user_question(UserQuestionRequest {
                    request_id,
                    local_thread_id,
                    questions,
                });
            }
            "currentTime/read" => {
                let current_time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                self.send_raw(json!({"id":request_id,"result":{"currentTimeAt":current_time}}));
            }
            _ => {
                self.activity_log
                    .push(format!("Unsupported Codex request: {method}"));
                self.send_raw(json!({
                    "id":request_id,
                    "error":{"code":-32601,"message":format!("CodeAgent does not support {method} yet")}
                }));
            }
        }
    }

    fn enqueue_approval(&mut self, approval: Approval) {
        if self.approval.is_none() {
            self.approval = Some(approval);
        } else {
            self.approval_queue.push_back(approval);
        }
    }

    fn enqueue_user_question(&mut self, request: UserQuestionRequest) {
        if self.user_question.is_none() {
            self.user_question = Some(request);
        } else {
            self.user_question_queue.push_back(request);
        }
    }

    fn apply_account(&mut self, result: &Value) {
        let Some(account) = result.get("account").filter(|v| !v.is_null()) else {
            self.account = AccountInfo {
                label: "Not signed in".into(),
                plan: "Run codex login".into(),
                authenticated: false,
            };
            self.plan_usage = None;
            return;
        };
        match account
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "chatgpt" => {
                self.account.label = account
                    .get("email")
                    .and_then(Value::as_str)
                    .unwrap_or("ChatGPT account")
                    .to_owned();
                self.account.plan = account
                    .get("planType")
                    .and_then(Value::as_str)
                    .unwrap_or("subscription")
                    .to_owned();
                self.account.authenticated = true;
            }
            "apiKey" => {
                self.account = AccountInfo {
                    label: "OpenAI API".into(),
                    plan: "API key".into(),
                    authenticated: true,
                };
            }
            other => {
                self.account = AccountInfo {
                    label: other.to_owned(),
                    plan: "configured".into(),
                    authenticated: true,
                };
            }
        }
    }

    fn refresh_plan_usage(&mut self) {
        if !self.connected
            || self.usage_loading
            || self
                .pending
                .values()
                .any(|call| matches!(call, PendingCall::RateLimits))
        {
            return;
        }
        self.usage_loading = true;
        self.usage_error = None;
        self.request(
            "account/rateLimits/read",
            Value::Null,
            PendingCall::RateLimits,
        );
    }

    fn consume_reset(&mut self, credit_id: Option<String>) {
        if self.reset_in_progress {
            return;
        }
        self.reset_in_progress = true;
        self.usage_error = None;
        let idempotency_key = format!("codeagent-{}-{}", unix_timestamp_millis(), self.next_id);
        self.request(
            "account/rateLimitResetCredit/consume",
            json!({
                "creditId": credit_id,
                "idempotencyKey": idempotency_key
            }),
            PendingCall::ConsumeReset,
        );
    }

    fn apply_models(&mut self, result: &Value) {
        self.models = result
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|m| !m.get("hidden").and_then(Value::as_bool).unwrap_or(false))
            .filter_map(|m| {
                let id = m.get("model").or_else(|| m.get("id"))?.as_str()?.to_owned();
                let efforts = m
                    .get("supportedReasoningEfforts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|e| {
                        e.get("reasoningEffort")
                            .or_else(|| e.get("effort"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect();
                Some(ModelOption {
                    id,
                    display_name: m
                        .get("displayName")
                        .and_then(Value::as_str)
                        .unwrap_or("Codex")
                        .to_owned(),
                    description: m
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    efforts,
                    default_effort: m
                        .get("defaultReasoningEffort")
                        .and_then(Value::as_str)
                        .unwrap_or("high")
                        .to_owned(),
                    is_default: m.get("isDefault").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect();
        if self.prefs.model.is_empty()
            && let Some(model) = self
                .models
                .iter()
                .find(|m| m.is_default)
                .or_else(|| self.models.first())
        {
            self.prefs.model.clone_from(&model.id);
            self.prefs.effort.clone_from(&model.default_effort);
        }
        if !self
            .models
            .iter()
            .any(|model| model.id == self.prefs.summary_model)
            && let Some(luna) = self
                .models
                .iter()
                .find(|model| model.display_name.to_ascii_lowercase().contains("5.6-luna"))
        {
            self.prefs.summary_model.clone_from(&luna.id);
        }
        if let Some(summary_model) = self
            .models
            .iter()
            .find(|model| model.id == self.prefs.summary_model)
            && !summary_model
                .efforts
                .iter()
                .any(|effort| effort == &self.prefs.summary_effort)
        {
            self.prefs.summary_effort = summary_model
                .efforts
                .iter()
                .find(|effort| effort.as_str() == "low")
                .or_else(|| summary_model.efforts.first())
                .cloned()
                .unwrap_or_else(|| "low".into());
        }
    }

    fn handle_summary_notification(&mut self, method: &str, params: &Value) -> bool {
        let Some(summary_thread_id) = params.get("threadId").and_then(Value::as_str) else {
            return false;
        };
        if !self.active_summaries.contains_key(summary_thread_id) {
            return false;
        }

        match method {
            "item/agentMessage/delta" => {
                if let Some(delta) = params.get("delta").and_then(Value::as_str)
                    && let Some(active) = self.active_summaries.get_mut(summary_thread_id)
                {
                    active.output.push_str(delta);
                }
            }
            "item/completed" => {
                if let Some(text) = params
                    .get("item")
                    .filter(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"))
                    .and_then(|item| item.get("text"))
                    .and_then(Value::as_str)
                    && !text.trim().is_empty()
                    && let Some(active) = self.active_summaries.get_mut(summary_thread_id)
                {
                    active.output = text.to_owned();
                }
            }
            "turn/started" => {
                if let Some(active) = self.active_summaries.get_mut(summary_thread_id) {
                    active.turn_id = params
                        .get("turn")
                        .and_then(|turn| turn.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
            }
            "turn/completed" => {
                let status = params
                    .get("turn")
                    .and_then(|turn| turn.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("completed");
                if let Some(active) = self.active_summaries.remove(summary_thread_id) {
                    if status != "failed" {
                        self.activity_log.push("Summary completed".into());
                        self.apply_summary_result(active);
                    } else {
                        self.activity_log.push("Summary generation failed".into());
                    }
                }
            }
            "warning" | "configWarning" | "error" => {
                if let Some(message) = params.get("message").and_then(Value::as_str)
                    && !is_skills_budget_warning(message)
                {
                    self.activity_log.push(format!("Summary: {message}"));
                }
            }
            _ => {}
        }
        true
    }

    fn apply_summary_result(&mut self, active: ActiveSummary) {
        let max_chars = match active.target {
            SummaryTarget::ThreadTitle { .. } => 52,
            SummaryTarget::ActivityGroup { .. } => 46,
        };
        let summary = clean_summary(&active.output, max_chars);
        if summary.is_empty() {
            return;
        }
        match active.target {
            SummaryTarget::ThreadTitle { local_thread_id } => {
                apply_generated_thread_title(&mut self.threads, &local_thread_id, summary);
            }
            SummaryTarget::ActivityGroup {
                local_thread_id,
                first_item_id,
            } => {
                if self.active_local_thread.as_deref() == Some(&local_thread_id) {
                    if let Some(item) = self
                        .conversation
                        .iter_mut()
                        .find(|item| item.id == first_item_id)
                    {
                        item.summary = Some(summary);
                    }
                    self.sync_active_conversation();
                } else if let Some(item) = self
                    .threads
                    .iter_mut()
                    .find(|thread| thread.id == local_thread_id)
                    .and_then(|thread| {
                        thread
                            .messages
                            .iter_mut()
                            .find(|item| item.id == first_item_id)
                    })
                {
                    item.summary = Some(summary);
                }
            }
        }
    }

    fn queue_missing_summaries(&mut self) {
        let threads = self.threads.clone();
        for thread in threads {
            if !thread.title_generated
                && let Some(first_request) = thread
                    .messages
                    .iter()
                    .find(|item| item.kind == ItemKind::User)
                    .map(|item| item.body.trim())
                    .filter(|request| !request.is_empty())
            {
                let key = format!("thread-title:{}", thread.id);
                self.queue_summary_job(SummaryJob {
                    key,
                    target: SummaryTarget::ThreadTitle {
                        local_thread_id: thread.id.clone(),
                    },
                    prompt: format!(
                        "Create a concise 3-7 word sidebar title for this request. Do not call tools. Return only the title, with no quotes, punctuation, or explanation.\n\nRequest:\n{}",
                        truncate_text(first_request, 2_000)
                    ),
                });
            }

            let mut index = 0;
            while index < thread.messages.len() {
                if !is_completed_activity(&thread.messages[index]) {
                    index += 1;
                    continue;
                }
                let start = index;
                while index < thread.messages.len()
                    && is_completed_activity(&thread.messages[index])
                {
                    index += 1;
                }
                if index - start < 2 || thread.messages[start].summary.is_some() {
                    continue;
                }
                let first_item_id = thread.messages[start].id.clone();
                let key = format!("activity:{}:{first_item_id}", thread.id);
                let operations = thread.messages[start..index]
                    .iter()
                    .map(activity_summary_source)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.queue_summary_job(SummaryJob {
                    key,
                    target: SummaryTarget::ActivityGroup {
                        local_thread_id: thread.id.clone(),
                        first_item_id,
                    },
                    prompt: format!(
                        "Summarize these related agent actions as a 2-5 word past-tense UI label. Describe the shared task, not the tools. Do not call tools or inspect files. Return only the label, with no quotes or punctuation.\n\nActions:\n{}",
                        truncate_text(&operations, 2_000)
                    ),
                });
            }
        }
    }

    fn queue_thread_title_summary(&mut self, local_thread_id: &str, request: &str) {
        let key = format!("thread-title:{local_thread_id}");
        self.queue_summary_job(SummaryJob {
            key,
            target: SummaryTarget::ThreadTitle {
                local_thread_id: local_thread_id.to_owned(),
            },
            prompt: format!(
                "Create a concise 3-7 word sidebar title for this request. Do not call tools. Return only the title, with no quotes, punctuation, or explanation.\n\nRequest:\n{}",
                truncate_text(request, 2_000)
            ),
        });
    }

    fn queue_summary_job(&mut self, job: SummaryJob) {
        if self.summary_pending_keys.insert(job.key.clone()) {
            self.summary_queue.push_back(job);
        }
    }

    fn start_queued_summaries(&mut self) {
        const MAX_CONCURRENT_SUMMARIES: usize = 4;
        let starting = self
            .pending
            .values()
            .filter(|pending| matches!(pending, PendingCall::SummaryThreadStart(_)))
            .count();
        let available =
            MAX_CONCURRENT_SUMMARIES.saturating_sub(self.active_summaries.len() + starting);
        for _ in 0..available {
            let Some(job) = self.summary_queue.pop_front() else {
                break;
            };
            self.request(
                "thread/start",
                json!({
                    "cwd":self.prefs.workspace,
                    "ephemeral":true,
                    "approvalPolicy":"never",
                    "sandbox":"read-only",
                    "serviceName":"codeagent-summary",
                    "model":self.prefs.summary_model
                }),
                PendingCall::SummaryThreadStart(job),
            );
        }
    }

    fn active_project_path(&self) -> Option<&str> {
        let active = self.active_project.as_deref()?;
        self.projects
            .iter()
            .find(|project| project.id == active)
            .map(|project| project.path.as_str())
    }

    fn refresh_threads(&mut self) {
        self.threads
            .sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
    }

    fn sync_active_conversation(&mut self) {
        let Some(id) = self.active_local_thread.as_deref() else {
            return;
        };
        if let Some(thread) = self.threads.iter_mut().find(|thread| thread.id == id) {
            sync_thread_messages(thread, &self.conversation);
        }
    }

    fn active_thread_busy(&self) -> bool {
        self.active_local_thread
            .as_ref()
            .is_some_and(|id| self.running_turns.contains_key(id))
    }

    fn active_context_usage(&self) -> Option<ContextWindowUsage> {
        self.active_local_thread
            .as_ref()
            .and_then(|id| self.threads.iter().find(|thread| &thread.id == id))
            .and_then(|thread| thread.context_usage)
    }

    fn context_for_active_thread(&self) -> String {
        let mut parts = Vec::new();
        for item in self.conversation.iter().rev().skip(1) {
            let role = match item.kind {
                ItemKind::User => "User",
                ItemKind::Assistant => "Assistant",
                _ => continue,
            };
            if !item.body.trim().is_empty() {
                parts.push(format!("{role}: {}", item.body.trim()));
            }
        }
        parts.reverse();
        let mut transcript = parts.join("\n\n");
        const MAX_CONTEXT_CHARS: usize = 60_000;
        if transcript.chars().count() > MAX_CONTEXT_CHARS {
            transcript = transcript
                .chars()
                .rev()
                .take(MAX_CONTEXT_CHARS)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
        }
        transcript
    }

    fn ingest_item(&mut self, local_thread_id: &str, item: &Value, completed: bool) {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown-item")
            .to_owned();
        let kind_name = item.get("type").and_then(Value::as_str).unwrap_or("system");
        let (kind, title, body) = match kind_name {
            "userMessage" => {
                let body = item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                (ItemKind::User, "You".to_owned(), body)
            }
            "agentMessage" => (
                ItemKind::Assistant,
                "Codex".into(),
                item.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            "reasoning" => {
                let mut parts = Vec::new();
                for field in ["summary", "content"] {
                    parts.extend(
                        item.get(field)
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned),
                    );
                }
                (ItemKind::Reasoning, "Reasoning".into(), parts.join("\n"))
            }
            "plan" => (
                ItemKind::Plan,
                "Plan".into(),
                item.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            "commandExecution" => {
                let command = item
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("Command");
                let output = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                (ItemKind::Command, command.to_owned(), output.to_owned())
            }
            "fileChange" => {
                let changes = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|c| {
                        let path = c.get("path").and_then(Value::as_str).unwrap_or("file");
                        let kind = c.get("kind").and_then(Value::as_str).unwrap_or("update");
                        format!("{kind}: {path}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (ItemKind::FileChange, "File changes".into(), changes)
            }
            "mcpToolCall" | "dynamicToolCall" | "collabAgentToolCall" => {
                let tool = item.get("tool").and_then(Value::as_str).unwrap_or("Tool");
                let body = item.get("arguments").map(value_to_text).unwrap_or_default();
                (ItemKind::Tool, tool.to_owned(), body)
            }
            "webSearch" => (ItemKind::Tool, "Web search".into(), value_to_text(item)),
            other => (ItemKind::System, humanize(other), value_to_text(item)),
        };

        let Some(messages) = self.messages_for_thread_mut(local_thread_id) else {
            return;
        };

        if kind == ItemKind::User
            && let Some(local) = messages
                .iter_mut()
                .rev()
                .find(|existing| existing.id.starts_with("local-user-"))
        {
            local.id = id;
            local.status = if completed { "completed" } else { "running" }.into();
            return;
        }
        if let Some(existing) = messages.iter_mut().find(|x| x.id == id) {
            existing.kind = kind;
            existing.title = title;
            if !body.is_empty() && existing.kind != ItemKind::User {
                existing.body = body;
            }
            existing.status = if completed { "completed" } else { "running" }.into();
            if completed
                && matches!(
                    existing.kind,
                    ItemKind::Command | ItemKind::Tool | ItemKind::FileChange | ItemKind::Plan
                )
            {
                existing.collapsed = true;
            }
        } else {
            let mut entry = ConversationItem::new(id, kind, title);
            entry.body = body;
            entry.status = if completed { "completed" } else { "running" }.into();
            entry.collapsed = matches!(
                entry.kind,
                ItemKind::Reasoning
                    | ItemKind::Command
                    | ItemKind::Tool
                    | ItemKind::FileChange
                    | ItemKind::Plan
            ) && completed;
            messages.push(entry);
        }
    }

    fn append_delta(&mut self, local_thread_id: &str, params: &Value, kind: ItemKind, title: &str) {
        let id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or("streaming")
            .to_owned();
        let delta = params
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let is_active = self.active_local_thread.as_deref() == Some(local_thread_id);
        let Some(messages) = self.messages_for_thread_mut(local_thread_id) else {
            return;
        };
        if let Some(item) = messages.iter_mut().find(|x| x.id == id) {
            item.body.push_str(delta);
        } else {
            let mut item = ConversationItem::new(id, kind, title);
            item.body.push_str(delta);
            messages.push(item);
        }
        if is_active {
            self.should_scroll = true;
        }
    }

    fn messages_for_thread_mut(
        &mut self,
        local_thread_id: &str,
    ) -> Option<&mut Vec<ConversationItem>> {
        if self.active_local_thread.as_deref() == Some(local_thread_id) {
            Some(&mut self.conversation)
        } else {
            self.threads
                .iter_mut()
                .find(|thread| thread.id == local_thread_id)
                .map(|thread| &mut thread.messages)
        }
    }

    fn send_prompt(&mut self) {
        let text = self.prompt.trim().to_owned();
        if text.is_empty()
            || self.active_thread_busy()
            || !self.connected
            || self.active_project.is_none()
        {
            return;
        }
        if self.active_local_thread.is_none() {
            self.new_thread();
        }
        let Some(local_thread_id) = self.active_local_thread.clone() else {
            return;
        };
        self.prompt.clear();
        let mut local = ConversationItem::new(
            format!("local-user-{}", self.next_id),
            ItemKind::User,
            "You",
        );
        local.body = text.clone();
        local.status = "completed".into();
        if !self.attachments.is_empty() {
            local
                .body
                .push_str(&format!("\n\n{} attachment(s)", self.attachments.len()));
        }
        self.conversation.push(local);
        if let Some(thread) = self
            .threads
            .iter_mut()
            .find(|thread| thread.id == local_thread_id)
        {
            // Recency is user-action based. Streaming deltas and merely opening a
            // thread must not continuously reshuffle the clickable sidebar rows.
            thread.updated_at = unix_timestamp();
            if thread.title == "New thread" {
                thread.title = text
                    .lines()
                    .next()
                    .unwrap_or("New thread")
                    .chars()
                    .take(70)
                    .collect();
            }
        }
        self.sync_active_conversation();
        self.queue_thread_title_summary(&local_thread_id, &text);
        self.start_queued_summaries();
        self.should_scroll = true;
        self.running_turns.insert(local_thread_id.clone(), None);

        let restore_context = !self.runtime_threads.contains_key(&local_thread_id);
        let mut turn_text = text;
        if restore_context {
            let transcript = self.context_for_active_thread();
            if !transcript.is_empty() {
                turn_text = format!(
                    "Continue this locally saved CodeAgent conversation. Use the transcript as context; do not repeat it in your answer.\n\n<conversation_history>\n{transcript}\n</conversation_history>\n\nCurrent user request:\n{turn_text}"
                );
            }
        }
        let mut input = vec![json!({"type":"text","text":turn_text,"text_elements":[]})];
        for path in self.attachments.drain(..) {
            input.push(json!({"type":"localImage","path":path}));
        }
        let turn = PendingTurn {
            input,
            cwd: self.prefs.workspace.clone(),
            approval_policy: self.prefs.approval.wire().to_owned(),
            sandbox: self.prefs.sandbox.wire().to_owned(),
            model: self.prefs.model.clone(),
            effort: self.prefs.effort.clone(),
        };

        if let Some(runtime_thread_id) = self.runtime_threads.get(&local_thread_id).cloned() {
            self.start_turn(local_thread_id, runtime_thread_id, turn);
        } else {
            let mut params = json!({
                "cwd":turn.cwd,
                "ephemeral":true,
                "approvalPolicy":turn.approval_policy,
                "sandbox":turn.sandbox,
                "serviceName":"codeagent"
            });
            if !turn.model.is_empty() {
                params["model"] = Value::String(turn.model.clone());
            }
            self.request(
                "thread/start",
                params,
                PendingCall::ThreadStart {
                    local_thread_id,
                    turn,
                },
            );
        }
    }

    fn start_turn(&mut self, local_thread_id: String, thread_id: String, turn: PendingTurn) {
        self.request(
            "turn/start",
            json!({
                "threadId":thread_id,
                "input":turn.input,
                "cwd":turn.cwd,
                "approvalPolicy":turn.approval_policy,
                "model":turn.model,
                "effort":turn.effort
            }),
            PendingCall::TurnStart { local_thread_id },
        );
    }

    fn interrupt(&mut self) {
        if let Some(local_thread_id) = self.active_local_thread.clone() {
            self.interrupt_thread(local_thread_id);
        }
    }

    fn interrupt_thread(&mut self, local_thread_id: String) {
        if let (Some(thread_id), Some(turn_id)) = (
            self.runtime_threads.get(&local_thread_id).cloned(),
            self.running_turns.get(&local_thread_id).cloned().flatten(),
        ) {
            self.request(
                "turn/interrupt",
                json!({"threadId":thread_id,"turnId":turn_id}),
                PendingCall::Interrupt { local_thread_id },
            );
        }
    }

    fn open_thread(&mut self, id: String) {
        if self.active_local_thread.as_deref() == Some(&id) {
            return;
        }
        self.sync_active_conversation();
        let Some(thread) = self.threads.iter().find(|thread| thread.id == id) else {
            return;
        };
        let project_id = thread.project_id.clone();
        let messages = thread.messages.clone();
        let Some(project_path) = self
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
        else {
            return;
        };
        self.active_project = Some(project_id);
        self.prefs.workspace = project_path;
        self.conversation = messages;
        self.active_local_thread = Some(id.clone());
        self.active_thread = self.runtime_threads.get(&id).cloned();
        self.active_turn = self.running_turns.get(&id).cloned().flatten();
        self.activity_log.push("Opened local conversation".into());
        self.should_scroll = true;
        self.refresh_workspace_info();
    }

    fn new_thread(&mut self) {
        if self.active_project.is_none() {
            return;
        }
        self.sync_active_conversation();
        let id = self.local_id("thread");
        let now = unix_timestamp();
        self.threads.push(LocalThread {
            id: id.clone(),
            project_id: self.active_project.clone().unwrap_or_default(),
            title: "New thread".into(),
            created_at: now,
            updated_at: now,
            title_generated: false,
            messages: Vec::new(),
            context_usage: None,
        });
        self.active_local_thread = Some(id);
        self.active_thread = None;
        self.active_turn = None;
        self.conversation.clear();
        self.prompt.clear();
        self.attachments.clear();
        self.activity_log.push("New conversation".into());
    }

    fn new_thread_for_project(&mut self, project_id: String) {
        self.select_project(project_id);
        self.new_thread();
    }

    fn archive_thread(&mut self, id: String) {
        if self.running_turns.contains_key(&id) {
            self.info("Stop this thread before archiving it".into());
            return;
        }
        self.threads.retain(|thread| thread.id != id);
        self.runtime_threads.remove(&id);
        if self.active_local_thread.as_deref() == Some(&id) {
            self.active_local_thread = None;
            self.active_thread = None;
            self.active_turn = None;
            self.conversation.clear();
        }
    }

    fn send_approval(&mut self, decision: &str) {
        let Some(approval) = self.approval.take() else {
            return;
        };
        let result = if approval.method == "execCommandApproval"
            || approval.method == "applyPatchApproval"
        {
            let legacy_decision = if decision == "acceptForSession" {
                json!("approved_for_session")
            } else if decision == "accept" {
                json!("approved")
            } else {
                json!({"denied":{"rejection":"Denied by user"}})
            };
            json!({"decision":legacy_decision})
        } else {
            json!({"decision":decision})
        };
        self.send_raw(json!({"id":approval.request_id,"result":result}));
        self.activity_log.push(format!("Approval: {decision}"));
        self.approval = self.approval_queue.pop_front();
    }

    fn send_question_answers(&mut self) {
        let Some(request) = self.user_question.take() else {
            return;
        };
        let answers: serde_json::Map<String, Value> = request
            .questions
            .into_iter()
            .map(|question| (question.id, json!({"answers":[question.answer]})))
            .collect();
        self.send_raw(json!({"id":request.request_id,"result":{"answers":answers}}));
        self.activity_log.push("Answered Codex question".into());
        self.user_question = self.user_question_queue.pop_front();
    }

    fn send_raw(&mut self, value: Value) {
        if let Some(backend) = &self.backend
            && let Err(err) = backend.send(value)
        {
            self.error(err);
        }
    }

    fn refresh_workspace_info(&mut self) {
        let Some(root) = self.active_project_path().map(str::to_owned) else {
            self.git_diff.clear();
            self.files.clear();
            self.workspace_rx = None;
            return;
        };
        let (tx, rx) = unbounded();
        self.workspace_rx = Some(rx);
        thread::spawn(move || {
            let diff = hidden_command("git")
                .args(["diff", "--stat", "--", "."])
                .current_dir(&root)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_else(|| "No Git changes".into());
            let mut files = Vec::new();
            collect_files(Path::new(&root), Path::new(&root), 0, &mut files);
            let _ = tx.send((diff, files));
        });
    }

    fn error(&mut self, message: String) {
        self.activity_log.push(format!("Error: {message}"));
        self.toast = Some((message, 7.0, true));
    }

    fn info(&mut self, message: String) {
        self.toast = Some((message, 4.0, false));
    }

    fn local_id(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}-{}-{}", unix_timestamp_millis(), self.next_id);
        self.next_id += 1;
        id
    }

    fn add_project(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_directory(&self.prefs.workspace)
            .pick_folder()
        {
            let path = path.to_string_lossy().to_string();
            if let Some(existing) = self
                .projects
                .iter()
                .find(|project| project.path.eq_ignore_ascii_case(&path))
            {
                let id = existing.id.clone();
                self.select_project(id);
                self.info("Project already added".into());
                return;
            }
            let name = workspace_name(&path).to_owned();
            let id = self.local_id("project");
            self.projects.push(Project {
                id: id.clone(),
                name,
                path,
                created_at: unix_timestamp(),
            });
            self.select_project(id);
            self.info("Project added".into());
        }
    }

    fn select_project(&mut self, id: String) {
        if self.active_project.as_deref() == Some(&id) {
            return;
        }
        self.sync_active_conversation();
        let Some(path) = self
            .projects
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.path.clone())
        else {
            return;
        };
        self.active_project = Some(id);
        self.active_local_thread = None;
        self.active_thread = None;
        self.active_turn = None;
        self.conversation.clear();
        self.prompt.clear();
        self.attachments.clear();
        self.prefs.workspace = path;
        self.refresh_workspace_info();
    }

    fn attach_files(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "gif"])
            .set_directory(&self.prefs.workspace)
            .pick_files()
        {
            self.attachments
                .extend(files.into_iter().map(|p| p.to_string_lossy().to_string()));
        }
    }

    fn keyboard_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::N)) {
            self.new_thread();
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)) {
            self.send_prompt();
        }
        if self.active_thread_busy()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.interrupt();
        }
    }

    fn open_settings(&mut self, category: SettingsCategory) {
        self.settings_category = category;
        self.show_settings = true;
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        let usage_left = self.plan_usage.as_ref().and_then(|usage| {
            usage
                .limits
                .iter()
                .flat_map(|limit| [&limit.primary, &limit.secondary])
                .flatten()
                .map(|window| 100_u32.saturating_sub(window.used_percent))
                .min()
        });
        egui::TopBottomPanel::top("top_bar")
            .exact_height(46.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(14, 7)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    brand_mark(ui, 20.0);
                    ui.label(RichText::new("CodeAgent").size(15.0).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let dot = if self.connected {
                            theme::SUCCESS
                        } else {
                            theme::WARNING
                        };
                        ui.label(
                            RichText::new(&self.connection_text)
                                .small()
                                .color(theme::MUTED),
                        );
                        status_dot(ui, dot, 8.0);
                        if let Some(remaining) = usage_left
                            && ui
                                .add(egui::Button::new(format!("{remaining}% usage left")).small())
                                .on_hover_text("Open Codex plan usage")
                                .clicked()
                        {
                            self.open_settings(SettingsCategory::Codex);
                        }
                    });
                });
            });
    }

    fn left_sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("left_sidebar_v2")
            .exact_width(252.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(8, 9)),
            )
            .show(ctx, |ui| {
                let can_create = self.active_project.is_some();
                if sidebar_action_button(
                    ui,
                    UiIcon::Add,
                    "New chat",
                    can_create,
                    Align::Min,
                    "Start a new chat",
                )
                .clicked()
                {
                    self.new_thread();
                }

                ui.add_space(5.0);
                ui.add(
                    TextEdit::singleline(&mut self.search)
                        .hint_text("Search")
                        .margin(egui::Margin::symmetric(8, 5))
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Projects").strong().color(theme::MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, UiIcon::Add, "Add project").clicked() {
                            self.add_project();
                        }
                        if icon_button(ui, UiIcon::Sort, "Sort by recent activity").clicked() {
                            self.refresh_threads();
                        }
                    });
                });
                ui.add_space(3.0);

                let projects = self.projects.clone();
                let threads = self.threads.clone();
                let needle = self.search.trim().to_lowercase();
                let footer_height = 53.0;
                let list_height = (ui.available_height() - footer_height).max(120.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), list_height),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ScrollArea::vertical()
                            .id_salt("project_thread_tree")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if projects.is_empty() {
                                    ui.add_space(14.0);
                                    ui.label(
                                        RichText::new("Add a project folder to get started")
                                            .small()
                                            .color(theme::MUTED),
                                    );
                                }
                                for project in projects {
                                    let mut project_threads: Vec<LocalThread> = threads
                                        .iter()
                                        .filter(|thread| thread.project_id == project.id)
                                        .filter(|thread| {
                                            needle.is_empty()
                                                || thread.title.to_lowercase().contains(&needle)
                                        })
                                        .cloned()
                                        .collect();
                                    if !needle.is_empty()
                                        && project_threads.is_empty()
                                        && !project.name.to_lowercase().contains(&needle)
                                    {
                                        continue;
                                    }
                                    project_threads
                                        .sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
                                    let project_id = project.id.clone();
                                    let project_name = project.name.clone();
                                    let header_id =
                                        ui.make_persistent_id(("project", &project.id));
                                    let (toggle_response, header_response, _) =
                                        egui::collapsing_header::CollapsingState::load_with_default_open(
                                            ui.ctx(),
                                            header_id,
                                            true,
                                        )
                                        .show_header(ui, |ui| {
                                            let title_width =
                                                (ui.available_width() - 30.0).max(24.0);
                                            let title_response = ui
                                                .add_sized(
                                                    [title_width, 22.0],
                                                    egui::Label::new(
                                                        RichText::new(&project_name)
                                                            .strong()
                                                            .size(13.0),
                                                    )
                                                    .truncate()
                                                    .sense(egui::Sense::click()),
                                                )
                                                .on_hover_text(&project_name);
                                            let new_chat_response = icon_button(
                                                ui,
                                                UiIcon::NewChat,
                                                &format!("New chat in {project_name}"),
                                            );
                                            (title_response, new_chat_response)
                                        })
                                        .body(|ui| {
                                        if project_threads.is_empty() {
                                            ui.label(
                                                RichText::new("No threads yet")
                                                    .small()
                                                    .color(theme::MUTED),
                                            );
                                        }
                                        for session in project_threads {
                                            let selected = self.active_local_thread.as_deref()
                                                == Some(&session.id);
                                            let running =
                                                self.running_turns.contains_key(&session.id);
                                            let fill = if selected {
                                                theme::ELEVATED
                                            } else {
                                                Color32::TRANSPARENT
                                            };
                                            let row = ui
                                                .push_id(("thread-row", &session.id), |ui| {
                                                    egui::Frame::new()
                                                        .fill(fill)
                                                        .corner_radius(CornerRadius::same(6))
                                                        .inner_margin(egui::Margin::symmetric(8, 6))
                                                        .show(ui, |ui| {
                                                            ui.set_width(ui.available_width());
                                                            ui.horizontal(|ui| {
                                                                let trailing_width =
                                                                    if running || selected {
                                                                        24.0
                                                                    } else {
                                                                        0.0
                                                                    };
                                                                let title_width = (ui
                                                                    .available_width()
                                                                    - trailing_width
                                                                    // Keep the row's horizontal content plus
                                                                    // the frame margins inside the fixed panel.
                                                                    - 20.0)
                                                                    .max(24.0);
                                                                ui.add_sized(
                                                                    [title_width, 20.0],
                                                                    egui::Label::new(
                                                                        RichText::new(
                                                                            &session.title,
                                                                        )
                                                                        .size(12.5)
                                                                        .color(if selected {
                                                                            theme::TEXT
                                                                        } else {
                                                                            theme::MUTED
                                                                        }),
                                                                    )
                                                                    .truncate(),
                                                                )
                                                                .on_hover_text(&session.title);
                                                                if running {
                                                                    ui.spinner();
                                                                } else if selected
                                                                    && icon_button(
                                                                        ui,
                                                                        UiIcon::Close,
                                                                        "Archive thread",
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.archive_thread(
                                                                        session.id.clone(),
                                                                    );
                                                                }
                                                            });
                                                        })
                                                        .response
                                                        .interact(egui::Sense::click())
                                                })
                                                .inner;
                                            if row.clicked() {
                                                self.open_thread(session.id.clone());
                                            }
                                            ui.add_space(1.0);
                                        }
                                        });
                                    let (title_response, new_chat_response) = header_response.inner;
                                    if new_chat_response.clicked() {
                                        self.new_thread_for_project(project_id.clone());
                                    } else if title_response.clicked() {
                                        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                                            ui.ctx(),
                                            header_id,
                                            true,
                                        );
                                        state.toggle(ui);
                                        state.store(ui.ctx());
                                        self.select_project(project_id);
                                    } else if toggle_response.clicked() {
                                        self.select_project(project_id);
                                    }
                                    ui.add_space(3.0);
                                }
                            });
                    },
                );

                ui.separator();
                if sidebar_action_button(
                    ui,
                    UiIcon::Settings,
                    "Settings",
                    true,
                    Align::Center,
                    "Open settings",
                )
                .clicked()
                {
                    self.open_settings(SettingsCategory::General);
                }
                ui.add_space(6.0);
            });
    }

    #[allow(dead_code)]
    fn left_sidebar_legacy(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("left_sidebar")
            .exact_width(258.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(egui::Margin::same(10)),
            )
            .show(ctx, |ui| {
                let can_create = self.active_project.is_some();
                if icon_text_button(
                    ui,
                    UiIcon::Add,
                    "New chat",
                    can_create,
                    Color32::TRANSPARENT,
                    Stroke::new(1.0_f32, theme::BORDER),
                    egui::vec2(ui.available_width(), 34.0),
                    "Start a new chat",
                )
                .clicked()
                {
                    self.new_thread();
                }
                ui.add_space(6.0);
                if icon_text_button(
                    ui,
                    UiIcon::Add,
                    "Add project",
                    true,
                    theme::PANEL_ALT,
                    Stroke::new(1.0_f32, theme::BORDER),
                    egui::vec2(ui.available_width(), 32.0),
                    "Add project",
                )
                .clicked()
                {
                    self.add_project();
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new("PROJECTS")
                        .small()
                        .strong()
                        .color(theme::MUTED),
                );
                let projects = self.projects.clone();
                ScrollArea::vertical()
                    .id_salt("project_scroll")
                    .max_height(150.0)
                    .show(ui, |ui| {
                        if projects.is_empty() {
                            ui.label(
                                RichText::new("Add a folder to get started")
                                    .small()
                                    .color(theme::MUTED),
                            );
                        }
                        for project in projects {
                            let selected = self.active_project.as_deref() == Some(&project.id);
                            if ui
                                .selectable_label(selected, &project.name)
                                .on_hover_text(&project.path)
                                .clicked()
                            {
                                self.select_project(project.id);
                            }
                        }
                    });
                ui.add_space(8.0);
                ui.add(
                    TextEdit::singleline(&mut self.search)
                        .hint_text("Search threads…")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("RECENT").small().strong().color(theme::MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, UiIcon::Refresh, "Refresh threads").clicked() {
                            self.refresh_threads();
                        }
                    });
                });
                ui.add_space(4.0);
                let needle = self.search.to_lowercase();
                let mut visible: Vec<LocalThread> = self
                    .threads
                    .iter()
                    .filter(|thread| {
                        self.active_project.as_deref() == Some(&thread.project_id)
                            && (needle.is_empty() || thread.title.to_lowercase().contains(&needle))
                    })
                    .cloned()
                    .collect();
                visible.sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
                ScrollArea::vertical()
                    .id_salt("session_scroll")
                    .show(ui, |ui| {
                        if visible.is_empty() {
                            ui.add_space(20.0);
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new("No conversations yet").color(theme::MUTED));
                                ui.label(
                                    RichText::new("Start a thread to see it here.")
                                        .small()
                                        .color(theme::MUTED),
                                );
                            });
                        }
                        for session in visible {
                            let selected = self.active_local_thread.as_deref() == Some(&session.id);
                            let fill = if selected {
                                theme::ACCENT_SOFT
                            } else {
                                Color32::TRANSPARENT
                            };
                            let response = egui::Frame::new()
                                .fill(fill)
                                .corner_radius(CornerRadius::same(7))
                                .inner_margin(egui::Margin::symmetric(9, 8))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        if selected && self.active_thread_busy() {
                                            status_dot(ui, theme::SUCCESS, 7.0);
                                        }
                                        ui.label(RichText::new(&session.title).size(13.0));
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                if icon_button(ui, UiIcon::Close, "Archive thread")
                                                    .clicked()
                                                {
                                                    self.archive_thread(session.id.clone());
                                                }
                                            },
                                        );
                                    });
                                })
                                .response
                                .interact(egui::Sense::click());
                            if response.clicked() {
                                self.open_thread(session.id.clone());
                            }
                            ui.add_space(2.0);
                        }
                    });
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    ui.separator();
                    ui.label(
                        RichText::new("History is stored by CodeAgent")
                            .small()
                            .color(theme::MUTED),
                    );
                    ui.label(RichText::new("Codex threads are ephemeral").small().color(
                        if self.account.authenticated {
                            theme::SUCCESS
                        } else {
                            theme::WARNING
                        },
                    ));
                });
            });
    }

    fn right_inspector(&mut self, ctx: &egui::Context) {
        if !self.prefs.show_inspector {
            return;
        }
        egui::SidePanel::right("right_inspector")
            .default_width(300.0)
            .min_width(240.0)
            .max_width(420.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(egui::Margin::same(10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (i, label) in ["Activity", "Changes", "Files"].iter().enumerate() {
                        if ui
                            .selectable_label(self.inspector_tab == i, *label)
                            .clicked()
                        {
                            self.inspector_tab = i;
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, UiIcon::Close, "Hide inspector").clicked() {
                            self.prefs.show_inspector = false;
                        }
                    });
                });
                ui.separator();
                match self.inspector_tab {
                    0 => ScrollArea::vertical().show(ui, |ui| {
                        for (index, line) in self.activity_log.iter().rev().enumerate() {
                            ui.horizontal_top(|ui| {
                                status_dot(
                                    ui,
                                    if index == 0 {
                                        theme::ACCENT
                                    } else {
                                        theme::MUTED
                                    },
                                    if index == 0 { 7.0 } else { 5.0 },
                                );
                                ui.label(RichText::new(line).small().color(if index == 0 {
                                    theme::TEXT
                                } else {
                                    theme::MUTED
                                }));
                            });
                            ui.add_space(5.0);
                        }
                    }),
                    1 => ScrollArea::both().show(ui, |ui| {
                        if self.git_diff.trim().is_empty() {
                            ui.label(RichText::new("Working tree is clean").color(theme::MUTED));
                        } else {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&self.git_diff).monospace().size(12.0),
                                )
                                .selectable(true),
                            );
                        }
                    }),
                    _ => ScrollArea::vertical().show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("{} files", self.files.len()))
                                .small()
                                .color(theme::MUTED),
                        );
                        ui.add_space(4.0);
                        for file in &self.files {
                            ui.label(RichText::new(format!("  {file}")).monospace().size(11.5));
                        }
                    }),
                };
            });
    }

    fn composer(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("composer")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(18, 10)),
            )
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.set_max_width(820.0);
                    if !self.attachments.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            let mut remove = None;
                            for (i, path) in self.attachments.iter().enumerate() {
                                let name = Path::new(path)
                                    .file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("image");
                                if attachment_button(ui, name).clicked() {
                                    remove = Some(i);
                                }
                            }
                            if let Some(i) = remove {
                                self.attachments.remove(i);
                            }
                        });
                    }
                    egui::Frame::new()
                        .fill(theme::PANEL_ALT)
                        .stroke(Stroke::new(1.0_f32, theme::BORDER))
                        .corner_radius(CornerRadius::same(14))
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            let edit = ui.add_sized(
                                [ui.available_width(), 58.0],
                                TextEdit::multiline(&mut self.prompt)
                                    .hint_text(
                                        RichText::new(if self.active_project.is_none() {
                                            "Add a project folder before starting a thread"
                                        } else if self.connected {
                                            "Ask for a follow-up or describe a task"
                                        } else {
                                            "Waiting for Codex CLI…"
                                        })
                                        .color(theme::MUTED),
                                    )
                                    .frame(false)
                                    .desired_rows(2),
                            );
                            if edit.has_focus()
                                && ui.input(|i| {
                                    i.key_pressed(egui::Key::Enter)
                                        && (i.modifiers.ctrl || i.modifiers.command)
                                })
                            {
                                self.send_prompt();
                            }
                            ui.horizontal(|ui| {
                                if icon_text_button(
                                    ui,
                                    UiIcon::Attachment,
                                    "Attach",
                                    true,
                                    theme::ELEVATED,
                                    Stroke::new(1.0_f32, theme::BORDER),
                                    egui::vec2(82.0, 30.0),
                                    "Attach images",
                                )
                                .clicked()
                                {
                                    self.attach_files();
                                }
                                egui::ComboBox::from_id_salt("model_picker")
                                    .selected_text(model_display(&self.models, &self.prefs.model))
                                    .width(135.0)
                                    .show_ui(ui, |ui| {
                                        for model in &self.models {
                                            ui.selectable_value(
                                                &mut self.prefs.model,
                                                model.id.clone(),
                                                &model.display_name,
                                            )
                                            .on_hover_text(&model.description);
                                        }
                                    });
                                let efforts = selected_efforts(&self.models, &self.prefs.model);
                                egui::ComboBox::from_id_salt("effort_picker")
                                    .selected_text(format!("{} effort", self.prefs.effort))
                                    .width(95.0)
                                    .show_ui(ui, |ui| {
                                        for effort in efforts {
                                            ui.selectable_value(
                                                &mut self.prefs.effort,
                                                effort.clone(),
                                                effort,
                                            );
                                        }
                                    });
                                egui::ComboBox::from_id_salt("sandbox_picker")
                                    .selected_text(self.prefs.sandbox.label())
                                    .width(95.0)
                                    .show_ui(ui, |ui| {
                                        for value in SandboxChoice::ALL {
                                            ui.selectable_value(
                                                &mut self.prefs.sandbox,
                                                value,
                                                value.label(),
                                            );
                                        }
                                    });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if self.active_thread_busy() {
                                        if icon_text_button(
                                            ui,
                                            UiIcon::Stop,
                                            "Stop",
                                            true,
                                            theme::ELEVATED,
                                            Stroke::new(1.0_f32, theme::DANGER),
                                            egui::vec2(68.0, 30.0),
                                            "Stop generation (Esc)",
                                        )
                                        .clicked()
                                        {
                                            self.interrupt();
                                        }
                                        ui.spinner();
                                    } else {
                                        let enabled = self.connected
                                            && self.active_project.is_some()
                                            && !self.prompt.trim().is_empty();
                                        if filled_icon_button(
                                            ui,
                                            UiIcon::Send,
                                            enabled,
                                            theme::ACCENT,
                                            Stroke::NONE,
                                            30.0,
                                            "Send message (Ctrl+Enter)",
                                        )
                                        .clicked()
                                        {
                                            self.send_prompt();
                                        }
                                    }
                                    if self.active_local_thread.is_some() {
                                        context_window_usage_ui(ui, self.active_context_usage());
                                    }
                                });
                            });
                        });
                });
            });
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG))
            .show(ctx, |ui| {
                if self.conversation.is_empty() {
                    self.welcome(ui);
                    return;
                }
                let title = self
                    .threads
                    .iter()
                    .find(|thread| self.active_local_thread.as_deref() == Some(&thread.id))
                    .map(|s| s.title.as_str())
                    .unwrap_or("New thread");
                egui::Frame::new()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(20, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(title).strong().size(16.0));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .add(
                                        egui::Button::new(if self.prefs.show_inspector {
                                            "Hide inspector"
                                        } else {
                                            "Show inspector"
                                        })
                                        .frame(false),
                                    )
                                    .clicked()
                                {
                                    self.prefs.show_inspector = !self.prefs.show_inspector;
                                }
                                ui.label(
                                    RichText::new(theme::short_path(&self.prefs.workspace, 45))
                                        .small()
                                        .color(theme::MUTED),
                                );
                            });
                        });
                    });
                ui.separator();
                ScrollArea::vertical()
                    .id_salt("conversation_scroll")
                    .auto_shrink([false; 2])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        let available = ui.available_width();
                        let content_width = available.min(820.0);
                        let left_gutter = ((available - content_width) * 0.5).max(0.0);
                        ui.horizontal_top(|ui| {
                            ui.add_space(left_gutter);
                            ui.vertical(|ui| {
                                ui.set_width(content_width);
                                ui.add_space(14.0);
                                let mut index = 0;
                                while index < self.conversation.len() {
                                    if is_completed_activity(&self.conversation[index]) {
                                        let mut end = index + 1;
                                        while end < self.conversation.len()
                                            && is_completed_activity(&self.conversation[end])
                                        {
                                            end += 1;
                                        }
                                        let count = end - index;
                                        if count >= 2 {
                                            let expanded = !self.conversation[index].collapsed;
                                            let summary = self.conversation[index]
                                                .summary
                                                .clone()
                                                .unwrap_or_else(|| {
                                                    fallback_activity_summary(
                                                        &self.conversation[index..end],
                                                    )
                                                });
                                            let response = ui.horizontal(|ui| {
                                                ui_icon(
                                                    ui,
                                                    if expanded {
                                                        UiIcon::ChevronDown
                                                    } else {
                                                        UiIcon::ChevronRight
                                                    },
                                                    15.0,
                                                    theme::MUTED,
                                                );
                                                ui.add(
                                                    egui::Button::new(
                                                        RichText::new(summary)
                                                            .size(12.5)
                                                            .color(theme::MUTED),
                                                    )
                                                    .frame(false),
                                                )
                                            });
                                            let response = response
                                                .inner
                                                .on_hover_text(format!("{count} grouped actions"));
                                            if response.clicked() {
                                                self.conversation[index].collapsed = expanded;
                                            }
                                            if expanded {
                                                ui.indent(("activity_group", index), |ui| {
                                                    for item in &mut self.conversation[index..end] {
                                                        draw_item(
                                                            ui,
                                                            item,
                                                            &mut self.markdown_cache,
                                                        );
                                                        ui.add_space(3.0);
                                                    }
                                                });
                                            }
                                            ui.add_space(6.0);
                                            index = end;
                                            continue;
                                        }
                                    }
                                    let item = &mut self.conversation[index];
                                    let hidden = item.kind == ItemKind::Reasoning
                                        && item.status != "running"
                                        && item.body.trim().is_empty();
                                    let compact = matches!(
                                        item.kind,
                                        ItemKind::Command
                                            | ItemKind::FileChange
                                            | ItemKind::Tool
                                            | ItemKind::Plan
                                            | ItemKind::System
                                    );
                                    draw_item(ui, item, &mut self.markdown_cache);
                                    if !hidden {
                                        ui.add_space(if compact { 4.0 } else { 14.0 });
                                    }
                                    index += 1;
                                }
                                if self.active_thread_busy() {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(
                                            RichText::new("Working…").small().color(theme::MUTED),
                                        );
                                    });
                                }
                                if self.should_scroll {
                                    ui.scroll_to_cursor(Some(Align::BOTTOM));
                                    self.should_scroll = false;
                                }
                                ui.add_space(20.0);
                            });
                        });
                    });
            });
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        let active_project = self
            .active_project
            .as_ref()
            .and_then(|id| self.projects.iter().find(|project| &project.id == id));
        let project_name = active_project
            .map(|project| project.name.clone())
            .unwrap_or_else(|| "No project selected".into());
        let project_path = active_project
            .map(|project| project.path.clone())
            .unwrap_or_else(|| "Add a project folder to start a thread".into());
        ui.vertical_centered(|ui| {
            ui.set_max_width(860.0);
            ui.add_space((ui.available_height() * 0.16).min(115.0));
            brand_mark(ui, 48.0);
            ui.heading("What should we build?");
            ui.label(
                RichText::new("A native control surface for your local Codex agent")
                    .size(15.0)
                    .color(theme::MUTED),
            );
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                let spacing = ui.spacing().item_spacing.x * 3.0;
                let suggestion_width = ((ui.available_width() - spacing) / 4.0).max(120.0);
                for suggestion in [
                    "Explain this codebase",
                    "Find and fix a bug",
                    "Add tests for this project",
                    "Review my current changes",
                ] {
                    if ui
                        .add_sized(
                            [suggestion_width, 42.0],
                            egui::Button::new(suggestion).fill(theme::PANEL_ALT),
                        )
                        .clicked()
                    {
                        self.prompt = suggestion.into();
                    }
                }
            });
            ui.add_space(18.0);
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(Stroke::new(1.0_f32, theme::BORDER))
                .corner_radius(CornerRadius::same(9))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("PROJECT").small().color(theme::ACCENT));
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&project_name).strong());
                            ui.label(
                                RichText::new(theme::short_path(&project_path, 65))
                                    .small()
                                    .color(theme::MUTED),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Add project").clicked() {
                                self.add_project();
                            }
                        });
                    });
                });
        });
    }

    fn approval_window(&mut self, ctx: &egui::Context) {
        let Some(approval) = self.approval.clone() else {
            return;
        };
        egui::Window::new(&approval.title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([560.0, 250.0])
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Codex is waiting for your approval.")
                        .color(theme::WARNING)
                        .strong(),
                );
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(theme::BG)
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(&approval.detail).monospace())
                                    .selectable(true),
                            );
                        });
                    });
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new("Allow once").fill(theme::ACCENT))
                        .clicked()
                    {
                        self.send_approval("accept");
                    }
                    if approval.allow_session && ui.button("Allow for session").clicked() {
                        self.send_approval("acceptForSession");
                    }
                    if ui
                        .button(RichText::new("Deny").color(theme::DANGER))
                        .clicked()
                    {
                        self.send_approval("decline");
                    }
                });
            });
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = self.show_settings;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([760.0, 680.0])
            .min_size([660.0, 520.0])
            .show(ctx, |ui| {
                let height = ui.available_height();
                ui.horizontal_top(|ui| {
                    egui::Frame::new()
                        .fill(theme::PANEL_ALT)
                        .corner_radius(CornerRadius::same(7))
                        .inner_margin(egui::Margin::symmetric(8, 10))
                        .show(ui, |ui| {
                            ui.set_min_size(egui::vec2(148.0, (height - 2.0).max(1.0)));
                            ui.set_max_width(148.0);
                            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                ui.label(
                                    RichText::new("SETTINGS")
                                        .small()
                                        .strong()
                                        .color(theme::MUTED),
                                );
                                ui.add_space(7.0);
                                for category in SettingsCategory::ALL {
                                    let selected = self.settings_category == category;
                                    if ui
                                        .add_sized(
                                            [ui.available_width(), 34.0],
                                            egui::Button::selectable(selected, category.label()),
                                        )
                                        .clicked()
                                    {
                                        self.settings_category = category;
                                    }
                                }
                            });
                        });
                    ui.add_space(8.0);
                    ui.allocate_ui_with_layout(
                        ui.available_size(),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ScrollArea::vertical()
                                .id_salt("settings_content")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    self.settings_category_ui(ctx, ui);
                                });
                        },
                    );
                });
            });
        self.show_settings = open;
    }

    fn settings_category_ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.heading(self.settings_category.label());
        ui.label(RichText::new(self.settings_category.description()).color(theme::MUTED));
        ui.add_space(18.0);

        match self.settings_category {
            SettingsCategory::General => self.general_settings_ui(ui),
            SettingsCategory::Agent => self.agent_settings_ui(ui),
            SettingsCategory::Summaries => self.summary_settings_ui(ui),
            SettingsCategory::Interface => self.interface_settings_ui(ui),
            SettingsCategory::Codex => self.codex_settings_ui(ctx, ui),
        }
    }

    fn general_settings_ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("general_settings_grid")
            .num_columns(2)
            .spacing([20.0, 12.0])
            .show(ui, |ui| {
                ui.label("Active project");
                ui.horizontal(|ui| {
                    ui.label(theme::short_path(&self.prefs.workspace, 42));
                    if ui.button("Add project...").clicked() {
                        self.add_project();
                    }
                });
                ui.end_row();
            });
    }

    fn agent_settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("Applied to new turns and persisted on this machine.")
                .color(theme::MUTED),
        );
        ui.add_space(12.0);
        egui::Grid::new("agent_settings_grid")
            .num_columns(2)
            .spacing([20.0, 12.0])
            .show(ui, |ui| {
                ui.label("Approval policy");
                egui::ComboBox::from_id_salt("settings_approval")
                    .selected_text(self.prefs.approval.label())
                    .show_ui(ui, |ui| {
                        for value in ApprovalChoice::ALL {
                            ui.selectable_value(&mut self.prefs.approval, value, value.label());
                        }
                    });
                ui.end_row();
                ui.label("Sandbox");
                egui::ComboBox::from_id_salt("settings_sandbox")
                    .selected_text(self.prefs.sandbox.label())
                    .show_ui(ui, |ui| {
                        for value in SandboxChoice::ALL {
                            ui.selectable_value(&mut self.prefs.sandbox, value, value.label());
                        }
                    });
                ui.end_row();
            });
    }

    fn summary_settings_ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("summary_settings_grid")
            .num_columns(2)
            .spacing([20.0, 12.0])
            .show(ui, |ui| {
                ui.label("Summary model");
                egui::ComboBox::from_id_salt("settings_summary_model")
                    .selected_text(model_display(&self.models, &self.prefs.summary_model))
                    .show_ui(ui, |ui| {
                        for model in &self.models {
                            ui.selectable_value(
                                &mut self.prefs.summary_model,
                                model.id.clone(),
                                &model.display_name,
                            );
                        }
                    });
                ui.end_row();
                ui.label("Summary effort");
                let summary_efforts = selected_efforts(&self.models, &self.prefs.summary_model);
                egui::ComboBox::from_id_salt("settings_summary_effort")
                    .selected_text(&self.prefs.summary_effort)
                    .show_ui(ui, |ui| {
                        for effort in summary_efforts {
                            ui.selectable_value(
                                &mut self.prefs.summary_effort,
                                effort.clone(),
                                effort,
                            );
                        }
                    });
                ui.end_row();
            });
    }

    fn interface_settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(&mut self.prefs.show_inspector, "Show activity panel");
    }

    fn codex_settings_ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Account").strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.connected && !self.usage_loading,
                        egui::Button::new("Refresh"),
                    )
                    .clicked()
                {
                    self.refresh_plan_usage();
                }
                if self.usage_loading {
                    ui.spinner();
                }
            });
        });
        ui.label(format!("Status: {}", self.connection_text));
        ui.label(if self.account.authenticated {
            format!(
                "Authenticated as {} ({})",
                self.account.label, self.account.plan
            )
        } else {
            "No authenticated Codex account detected".into()
        });
        ui.add_space(14.0);
        self.plan_usage_ui(ui);
        ui.add_space(8.0);
        if !self.connected && ui.button("Restart Codex").clicked() {
            self.start_backend(ctx);
        }
    }

    fn plan_usage_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = &self.usage_error {
            ui.label(RichText::new(error).color(theme::DANGER));
        }
        let Some(usage) = self.plan_usage.clone() else {
            if self.account.authenticated && !self.usage_loading {
                ui.label(RichText::new("Plan usage is not available.").color(theme::MUTED));
            }
            return;
        };

        ui.label(RichText::new("Plan usage").strong());
        if usage.limits.is_empty() {
            ui.label(RichText::new("No metered Codex limits were returned.").color(theme::MUTED));
        }
        for limit in &usage.limits {
            egui::Frame::new()
                .fill(theme::PANEL_ALT)
                .stroke(Stroke::new(1.0_f32, theme::BORDER))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    let name = if limit.name.eq_ignore_ascii_case("codex") {
                        "Codex".to_owned()
                    } else {
                        limit.name.clone()
                    };
                    ui.label(RichText::new(name).strong());
                    if let Some(window) = &limit.primary {
                        usage_window_ui(ui, window);
                    }
                    if let Some(window) = &limit.secondary {
                        ui.add_space(7.0);
                        usage_window_ui(ui, window);
                    }
                    if limit.credits_unlimited {
                        ui.add_space(7.0);
                        ui.label("Credits: unlimited");
                    } else if let Some(balance) = &limit.credits_balance {
                        ui.add_space(7.0);
                        ui.label(format!("Credit balance: {balance}"));
                    }
                });
            ui.add_space(8.0);
        }

        ui.add_space(2.0);
        ui.label(RichText::new(format!("Usage resets ({})", usage.available_reset_count)).strong());
        match &usage.reset_credits {
            Some(credits) if credits.is_empty() => {
                ui.label(RichText::new("No usage resets are available.").color(theme::MUTED));
            }
            Some(credits) => {
                for credit in credits {
                    self.reset_credit_ui(ui, credit);
                    ui.add_space(6.0);
                }
                if credits.len() < usage.available_reset_count as usize {
                    ui.label(
                        RichText::new(format!(
                            "Showing {} of {} available resets returned by Codex.",
                            credits.len(),
                            usage.available_reset_count
                        ))
                        .small()
                        .color(theme::MUTED),
                    );
                }
            }
            None if usage.available_reset_count > 0 => {
                ui.label(
                    RichText::new(format!(
                        "{} resets available; Codex did not return individual details.",
                        usage.available_reset_count
                    ))
                    .color(theme::MUTED),
                );
                if ui
                    .add_enabled(!self.reset_in_progress, egui::Button::new("Use next reset"))
                    .clicked()
                {
                    self.reset_confirmation = Some(ResetConfirmation {
                        credit_id: None,
                        title: "the next available usage reset".into(),
                    });
                }
            }
            None => {
                ui.label(RichText::new("No usage resets are available.").color(theme::MUTED));
            }
        }
    }

    fn reset_credit_ui(&mut self, ui: &mut egui::Ui, credit: &ResetCredit) {
        let expired = credit
            .expires_at
            .is_some_and(|expires_at| expires_at <= unix_timestamp());
        egui::Frame::new()
            .fill(theme::PANEL_ALT)
            .stroke(Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&credit.title).strong());
                        if !credit.description.is_empty() {
                            ui.label(
                                RichText::new(&credit.description)
                                    .small()
                                    .color(theme::MUTED),
                            );
                        }
                        ui.label(
                            RichText::new(format!(
                                "{}{}",
                                match credit.expires_at {
                                    Some(timestamp) if expired => {
                                        format!("Expired {}", format_timestamp(timestamp))
                                    }
                                    Some(timestamp) => {
                                        format!("Expires {}", format_timestamp(timestamp))
                                    }
                                    None => "Does not expire".into(),
                                },
                                if credit.status == "available" {
                                    String::new()
                                } else {
                                    format!(" · {}", humanize(&credit.status))
                                }
                            ))
                            .small()
                            .color(if expired {
                                theme::DANGER
                            } else {
                                theme::MUTED
                            }),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let can_use =
                            credit.status == "available" && !expired && !self.reset_in_progress;
                        if ui
                            .add_enabled(can_use, egui::Button::new("Use reset"))
                            .clicked()
                        {
                            self.reset_confirmation = Some(ResetConfirmation {
                                credit_id: Some(credit.id.clone()),
                                title: credit.title.clone(),
                            });
                        }
                    });
                });
            });
    }

    fn reset_confirmation_window(&mut self, ctx: &egui::Context) {
        let Some(confirmation) = self.reset_confirmation.take() else {
            return;
        };
        let mut open = true;
        let mut use_reset = false;
        let mut cancel = false;
        egui::Window::new("Use Codex usage reset?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([460.0, 180.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "Use {} now? This consumes the reset and cannot be undone.",
                    confirmation.title
                ));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new("Use reset").fill(theme::ACCENT))
                        .clicked()
                    {
                        use_reset = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if use_reset {
            self.consume_reset(confirmation.credit_id);
        } else if open && !cancel {
            self.reset_confirmation = Some(confirmation);
        }
    }

    fn question_window(&mut self, ctx: &egui::Context) {
        let Some(mut request) = self.user_question.take() else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        egui::Window::new("Codex needs your input")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([580.0, 360.0])
            .show(ctx, |ui| {
                ScrollArea::vertical().max_height(285.0).show(ui, |ui| {
                    for question in &mut request.questions {
                        ui.label(
                            RichText::new(&question.header)
                                .small()
                                .strong()
                                .color(theme::ACCENT),
                        );
                        ui.label(RichText::new(&question.question).strong());
                        ui.add_space(5.0);
                        if question.options.is_empty() {
                            ui.add(
                                TextEdit::singleline(&mut question.answer)
                                    .hint_text("Type your answer...")
                                    .password(question.secret)
                                    .desired_width(f32::INFINITY),
                            );
                        } else {
                            for (label, description) in &question.options {
                                ui.radio_value(&mut question.answer, label.clone(), label);
                                if !description.is_empty() {
                                    ui.indent(format!("desc-{label}"), |ui| {
                                        ui.label(
                                            RichText::new(description).small().color(theme::MUTED),
                                        );
                                    });
                                }
                            }
                        }
                        ui.add_space(14.0);
                    }
                });
                ui.separator();
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new("Submit").fill(theme::ACCENT))
                        .clicked()
                    {
                        submit = true;
                    }
                    if ui.button("Cancel turn").clicked() {
                        cancel = true;
                    }
                });
            });
        self.user_question = Some(request);
        if submit {
            self.send_question_answers();
        } else if cancel && let Some(request) = self.user_question.take() {
            let local_thread_id = request.local_thread_id.clone();
            self.send_raw(json!({
                "id":request.request_id,
                "error":{"code":-32001,"message":"User cancelled"}
            }));
            self.user_question = self.user_question_queue.pop_front();
            if let Some(local_thread_id) = local_thread_id {
                self.interrupt_thread(local_thread_id);
            }
        }
    }

    fn toast(&mut self, ctx: &egui::Context) {
        let Some((message, remaining, is_error)) = self.toast.clone() else {
            return;
        };
        let dt = ctx.input(|i| i.stable_dt).min(0.1) as f64;
        if remaining <= 0.0 {
            self.toast = None;
            return;
        }
        self.toast = Some((message.clone(), remaining - dt, is_error));
        ctx.request_repaint();
        egui::Area::new(egui::Id::new("toast"))
            .anchor(egui::Align2::RIGHT_TOP, [-18.0, 68.0])
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::ELEVATED)
                    .stroke(Stroke::new(
                        1.0_f32,
                        if is_error {
                            theme::DANGER
                        } else {
                            theme::SUCCESS
                        },
                    ))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(440.0);
                        ui.label(RichText::new(message).color(if is_error {
                            theme::DANGER
                        } else {
                            theme::TEXT
                        }));
                    });
            });
    }
}

impl App for CodeAgentApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        self.process_startup();
        self.process_backend(ctx);
        self.keyboard_shortcuts(ctx);
        self.top_bar(ctx);
        self.left_sidebar(ctx);
        self.right_inspector(ctx);
        self.composer(ctx);
        self.central(ctx);
        self.approval_window(ctx);
        self.question_window(ctx);
        self.settings_window(ctx);
        self.reset_confirmation_window(ctx);
        self.toast(ctx);
        if !self.connected {
            // Poll the app-server handshake promptly; its reader is intentionally
            // decoupled from the UI thread and cannot wake egui directly.
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        } else if !self.running_turns.is_empty() || !self.active_summaries.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Do no process creation or history parsing until the first UI frame is ready.
        if self.startup_deferred {
            self.startup_deferred = false;
            self.begin_deferred_startup(ctx);
            ctx.request_repaint();
        }
    }

    fn save(&mut self, storage: &mut dyn Storage) {
        self.sync_active_conversation();
        eframe::set_value(storage, PREFS_KEY, &self.prefs);
        // If the process exits before the background load completes, leave the prior
        // history value intact instead of replacing it with the temporary empty state.
        if self.history_loaded {
            eframe::set_value(
                storage,
                HISTORY_KEY,
                &AppHistory {
                    projects: self.projects.clone(),
                    threads: self.threads.clone(),
                    active_project: self.active_project.clone(),
                    active_thread: self.active_local_thread.clone(),
                },
            );
        }
    }
}

fn is_completed_activity(item: &ConversationItem) -> bool {
    item.status != "running"
        && matches!(
            item.kind,
            ItemKind::Command
                | ItemKind::FileChange
                | ItemKind::Tool
                | ItemKind::Plan
                | ItemKind::System
        )
}

fn activity_summary_source(item: &ConversationItem) -> String {
    let kind = match item.kind {
        ItemKind::Command => "Command",
        ItemKind::FileChange => "File change",
        ItemKind::Tool => "Tool",
        ItemKind::Plan => "Plan",
        _ => "Action",
    };
    let detail = if item.kind == ItemKind::FileChange && !item.body.trim().is_empty() {
        format!("{} — {}", item.title, item.body.trim())
    } else {
        item.title.clone()
    };
    format!("{kind}: {}", truncate_text(&detail, 160))
}

fn fallback_activity_summary(items: &[ConversationItem]) -> String {
    let text = items
        .iter()
        .map(|item| format!("{} {}", item.title, item.body))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let has_file_changes = items.iter().any(|item| item.kind == ItemKind::FileChange);
    let has_web_search = items
        .iter()
        .any(|item| item.kind == ItemKind::Tool && item.title.eq_ignore_ascii_case("web search"));
    let has_checks = ["cargo test", "cargo check", "cargo clippy", "cargo fmt"]
        .iter()
        .any(|needle| text.contains(needle));
    let has_build = text.contains("cargo build");
    let has_dependency_work = ["cargo metadata", "cargo audit", "rustsec", "advisories"]
        .iter()
        .any(|needle| text.contains(needle));
    let has_git_work = ["git diff", "git status", "git log"]
        .iter()
        .any(|needle| text.contains(needle));
    let has_file_inspection = [
        "get-content",
        "get-childitem",
        "rg ",
        "rg --files",
        "select-string",
    ]
    .iter()
    .any(|needle| text.contains(needle));

    if has_file_changes {
        "Edited project files".into()
    } else if has_web_search {
        "Searched the web".into()
    } else if has_dependency_work {
        "Checked project dependencies".into()
    } else if has_checks && has_build {
        "Built and checked project".into()
    } else if has_checks {
        "Ran project checks".into()
    } else if has_build {
        "Built the project".into()
    } else if has_git_work {
        "Reviewed working changes".into()
    } else if has_file_inspection {
        "Inspected project files".into()
    } else if items.iter().any(|item| item.kind == ItemKind::Plan) {
        "Updated implementation plan".into()
    } else if items.iter().any(|item| item.kind == ItemKind::Tool) {
        "Used development tools".into()
    } else {
        "Ran shell commands".into()
    }
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let mut value: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        value.push('…');
    }
    value
}

fn clean_summary(raw: &str, max_chars: usize) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let cleaned = line
        .trim_matches(&['"', '\'', '`', '*', '#', ' '][..])
        .trim_end_matches(&['.', ',', ':', ';', '!', '?'][..])
        .trim();
    truncate_text(cleaned, max_chars)
}

fn is_skills_budget_warning(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("skill descriptions were shortened")
        || (message.contains("skills context budget") && message.contains("shortened"))
}

fn draw_item(ui: &mut egui::Ui, item: &mut ConversationItem, markdown_cache: &mut CommonMarkCache) {
    match item.kind {
        ItemKind::User => {
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                let bubble_frame = egui::Frame::new()
                    .fill(theme::USER_BUBBLE)
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .corner_radius(CornerRadius::same(14))
                    .inner_margin(egui::Margin::symmetric(13, 10));
                let max_content_width =
                    (ui.available_width() - bubble_frame.total_margin().sum().x).clamp(1.0, 620.0);
                let content_width = if item.body.contains(['\r', '\n']) {
                    max_content_width
                } else {
                    ui.painter()
                        .layout_no_wrap(item.body.clone(), FontId::proportional(13.5), theme::TEXT)
                        .size()
                        .x
                        .min(max_content_width)
                };

                bubble_frame.show(ui, |ui| {
                    ui.set_width(content_width);
                    ui.with_layout(Layout::left_to_right(Align::TOP), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&item.body).size(13.5))
                                .selectable(true)
                                .wrap(),
                        );
                    });
                });
            });
        }
        ItemKind::Assistant => {
            draw_markdown(ui, markdown_cache, &item.body);
        }
        ItemKind::Reasoning => {
            if !item.body.trim().is_empty() {
                draw_markdown(ui, markdown_cache, &item.body);
            } else if item.status == "running" {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Thinking…").small().color(theme::MUTED));
                });
            }
        }
        _ => {
            let running = item.status == "running";
            let label = match item.kind {
                ItemKind::Command if running => "Running command".to_owned(),
                ItemKind::Command => "Ran command".to_owned(),
                ItemKind::FileChange if running => "Editing files".to_owned(),
                ItemKind::FileChange => "Edited files".to_owned(),
                ItemKind::Tool if item.title.eq_ignore_ascii_case("web search") => {
                    "Searched the web".to_owned()
                }
                ItemKind::Tool if running => format!("Using {}", item.title),
                ItemKind::Tool => format!("Used {}", item.title),
                ItemKind::Plan => "Updated plan".to_owned(),
                _ => item.title.clone(),
            };
            let has_details = !item.body.trim().is_empty()
                || matches!(item.kind, ItemKind::Command | ItemKind::FileChange);
            ui.horizontal(|ui| {
                ui_icon(
                    ui,
                    if has_details {
                        if item.collapsed {
                            UiIcon::ChevronRight
                        } else {
                            UiIcon::ChevronDown
                        }
                    } else {
                        UiIcon::Bullet
                    },
                    15.0,
                    theme::MUTED,
                );
                let response = ui
                    .add(
                        egui::Button::new(RichText::new(label).size(12.5).color(theme::MUTED))
                            .frame(false),
                    )
                    .on_hover_text(&item.title);
                if has_details && response.clicked() {
                    item.collapsed = !item.collapsed;
                }
                if running {
                    ui.spinner();
                }
            });
            if has_details && !item.collapsed {
                ui.add_space(2.0);
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(egui::Margin::symmetric(11, 8))
                    .show(ui, |ui| {
                        if item.kind == ItemKind::Command {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&item.title)
                                        .font(FontId::monospace(11.5))
                                        .color(theme::MUTED),
                                )
                                .selectable(true)
                                .wrap(),
                            );
                            if !item.body.trim().is_empty() {
                                ui.add_space(7.0);
                            }
                        }
                        if !item.body.trim().is_empty() {
                            ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&item.body)
                                            .font(FontId::monospace(11.5))
                                            .color(theme::TEXT),
                                    )
                                    .selectable(true)
                                    .wrap(),
                                );
                            });
                        }
                    });
            }
        }
    }
}

#[derive(Clone, Copy)]
enum UiIcon {
    Add,
    Attachment,
    Bullet,
    ChevronDown,
    ChevronRight,
    Close,
    NewChat,
    Refresh,
    Send,
    Settings,
    Sort,
    Stop,
}

fn ui_icon(ui: &mut egui::Ui, icon: UiIcon, size: f32, color: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    paint_ui_icon(ui.painter(), rect, icon, color);
    response
}

fn icon_button(ui: &mut egui::Ui, icon: UiIcon, tooltip: &str) -> egui::Response {
    let response = ui
        .add(
            egui::Button::new("")
                .frame(false)
                .min_size(egui::vec2(22.0, 22.0)),
        )
        .on_hover_text(tooltip);
    let color = ui.style().interact(&response).fg_stroke.color;
    paint_ui_icon(ui.painter(), response.rect.shrink(4.0), icon, color);
    response
}

fn sidebar_action_button(
    ui: &mut egui::Ui,
    icon: UiIcon,
    text: &str,
    enabled: bool,
    alignment: Align,
    tooltip: &str,
) -> egui::Response {
    let response = ui
        .add_enabled(
            enabled,
            egui::Button::new("")
                .frame(true)
                .frame_when_inactive(false)
                .corner_radius(CornerRadius::same(6))
                .min_size(egui::vec2(ui.available_width(), 34.0)),
        )
        .on_hover_text(tooltip);

    let color = ui.style().interact(&response).fg_stroke.color;
    let font = FontId::proportional(13.5);
    let text_width = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), color)
        .size()
        .x;
    let icon_size = 15.0;
    let gap = 7.0;
    let group_width = icon_size + gap + text_width;
    let group_left = match alignment {
        Align::Min => response.rect.left() + 8.0,
        Align::Center => response.rect.center().x - group_width * 0.5,
        Align::Max => response.rect.right() - group_width - 8.0,
    };
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(group_left + icon_size * 0.5, response.rect.center().y),
        egui::vec2(icon_size, icon_size),
    );
    paint_ui_icon(ui.painter(), icon_rect, icon, color);
    ui.painter().text(
        egui::pos2(group_left + icon_size + gap, response.rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        font,
        color,
    );

    response
}

fn filled_icon_button(
    ui: &mut egui::Ui,
    icon: UiIcon,
    enabled: bool,
    fill: Color32,
    stroke: Stroke,
    size: f32,
    tooltip: &str,
) -> egui::Response {
    let response = ui
        .add_enabled(
            enabled,
            egui::Button::new("")
                .fill(fill)
                .stroke(stroke)
                .corner_radius(CornerRadius::same((size * 0.5) as u8))
                .min_size(egui::vec2(size, size)),
        )
        .on_hover_text(tooltip);
    let color = ui.style().interact(&response).fg_stroke.color;
    paint_ui_icon(ui.painter(), response.rect.shrink(size * 0.25), icon, color);
    response
}

#[allow(clippy::too_many_arguments)]
fn icon_text_button(
    ui: &mut egui::Ui,
    icon: UiIcon,
    text: &str,
    enabled: bool,
    fill: Color32,
    stroke: Stroke,
    min_size: egui::Vec2,
    tooltip: &str,
) -> egui::Response {
    let font = FontId::proportional(13.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT)
        .size()
        .x;
    let icon_size = 13.0;
    let gap = 6.0;
    let desired_size = egui::vec2(
        min_size.x.max(text_width + icon_size + gap + 20.0),
        min_size.y.max(28.0),
    );
    let response = ui
        .add_enabled(
            enabled,
            egui::Button::new("")
                .fill(fill)
                .stroke(stroke)
                .min_size(desired_size),
        )
        .on_hover_text(tooltip);
    let color = ui.style().interact(&response).fg_stroke.color;
    let group_width = icon_size + gap + text_width;
    let group_left = response.rect.center().x - group_width * 0.5;
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(group_left + icon_size * 0.5, response.rect.center().y),
        egui::vec2(icon_size, icon_size),
    );
    paint_ui_icon(ui.painter(), icon_rect, icon, color);
    ui.painter().text(
        egui::pos2(group_left + icon_size + gap, response.rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        font,
        color,
    );
    response
}

fn attachment_button(ui: &mut egui::Ui, name: &str) -> egui::Response {
    let font = FontId::proportional(13.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font.clone(), theme::TEXT)
        .size()
        .x;
    let icon_size = 12.0;
    let gap = 6.0;
    let group_width = text_width + icon_size * 2.0 + gap * 2.0;
    let response = ui
        .add(egui::Button::new("").min_size(egui::vec2(group_width + 18.0, 28.0)))
        .on_hover_text("Remove attachment");
    let color = ui.style().interact(&response).fg_stroke.color;
    let left = response.rect.center().x - group_width * 0.5;
    let icon_rect = |x: f32| {
        egui::Rect::from_center_size(
            egui::pos2(x + icon_size * 0.5, response.rect.center().y),
            egui::vec2(icon_size, icon_size),
        )
    };
    paint_ui_icon(ui.painter(), icon_rect(left), UiIcon::Attachment, color);
    ui.painter().text(
        egui::pos2(left + icon_size + gap, response.rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        font,
        color,
    );
    paint_ui_icon(
        ui.painter(),
        icon_rect(left + icon_size + gap + text_width + gap),
        UiIcon::Close,
        color,
    );
    response
}

fn paint_ui_icon(painter: &egui::Painter, rect: egui::Rect, icon: UiIcon, color: Color32) {
    let center = rect.center();
    let size = rect.width().min(rect.height());
    let point = |x: f32, y: f32| center + egui::vec2(x * size, y * size);
    let stroke = Stroke::new((size * 0.1).clamp(1.2, 1.8), color);

    match icon {
        UiIcon::Add => {
            painter.line_segment([point(-0.3, 0.0), point(0.3, 0.0)], stroke);
            painter.line_segment([point(0.0, -0.3), point(0.0, 0.3)], stroke);
        }
        UiIcon::Attachment => {
            // A compact version of the familiar diagonal paperclip silhouette.
            // Keeping both loops open makes it remain legible at toolbar sizes.
            let clip_point =
                |x: f32, y: f32| center + egui::vec2((x - 12.0) / 24.0, (y - 12.0) / 24.0) * size;
            let line = |from: (f32, f32), to: (f32, f32)| {
                painter.line_segment([clip_point(from.0, from.1), clip_point(to.0, to.1)], stroke);
            };
            let curve = |points: [(f32, f32); 4]| {
                painter.add(egui::Shape::CubicBezier(
                    egui::epaint::CubicBezierShape::from_points_stroke(
                        points.map(|(x, y)| clip_point(x, y)),
                        false,
                        Color32::TRANSPARENT,
                        stroke,
                    ),
                ));
            };

            line((21.4, 11.1), (12.3, 20.2));
            curve([(12.3, 20.2), (9.9, 22.6), (6.1, 22.6), (3.8, 20.2)]);
            curve([(3.8, 20.2), (1.4, 17.9), (1.4, 14.1), (3.8, 11.8)]);
            line((3.8, 11.8), (13.0, 2.6));
            curve([(13.0, 2.6), (14.5, 1.0), (17.1, 1.0), (18.6, 2.6)]);
            curve([(18.6, 2.6), (20.2, 4.1), (20.2, 6.7), (18.6, 8.2)]);
            line((18.6, 8.2), (9.4, 17.4));
            curve([(9.4, 17.4), (8.6, 18.2), (7.4, 18.2), (6.6, 17.4)]);
            curve([(6.6, 17.4), (5.8, 16.6), (5.8, 15.4), (6.6, 14.6)]);
            line((6.6, 14.6), (15.1, 6.1));
        }
        UiIcon::Bullet => {
            painter.circle_filled(center, (size * 0.12).max(1.5), color);
        }
        UiIcon::ChevronDown => {
            painter.line_segment([point(-0.27, -0.12), point(0.0, 0.16)], stroke);
            painter.line_segment([point(0.0, 0.16), point(0.27, -0.12)], stroke);
        }
        UiIcon::ChevronRight => {
            painter.line_segment([point(-0.12, -0.27), point(0.16, 0.0)], stroke);
            painter.line_segment([point(0.16, 0.0), point(-0.12, 0.27)], stroke);
        }
        UiIcon::Close => {
            painter.line_segment([point(-0.25, -0.25), point(0.25, 0.25)], stroke);
            painter.line_segment([point(0.25, -0.25), point(-0.25, 0.25)], stroke);
        }
        UiIcon::NewChat => {
            let compose_box = egui::Rect::from_min_max(point(-0.35, -0.27), point(0.22, 0.34));
            painter.rect_stroke(
                compose_box,
                CornerRadius::same(2),
                stroke,
                egui::StrokeKind::Middle,
            );

            let pencil_start = point(-0.08, 0.11);
            let pencil_end = point(0.34, -0.31);
            let pencil_tip = point(-0.18, 0.21);
            let direction = (pencil_end - pencil_start).normalized();
            let normal = egui::vec2(-direction.y, direction.x) * size * 0.065;
            painter.line_segment([pencil_start + normal, pencil_end + normal], stroke);
            painter.line_segment([pencil_start - normal, pencil_end - normal], stroke);
            painter.line_segment([pencil_end + normal, pencil_end - normal], stroke);
            painter.line_segment([pencil_start + normal, pencil_tip], stroke);
            painter.line_segment([pencil_tip, pencil_start - normal], stroke);
        }
        UiIcon::Refresh => {
            let start = -0.55_f32;
            let sweep = std::f32::consts::TAU * 0.78;
            let radius = size * 0.3;
            let points = (0..=18).map(|step| {
                let angle = start + sweep * step as f32 / 18.0;
                center + egui::vec2(angle.cos(), angle.sin()) * radius
            });
            painter.add(egui::Shape::line(points.collect(), stroke));
            let end = start + sweep;
            let tip = center + egui::vec2(end.cos(), end.sin()) * radius;
            let tangent = egui::vec2(-end.sin(), end.cos());
            let inward = (center - tip).normalized();
            painter.line_segment([tip, tip + (tangent + inward) * size * 0.17], stroke);
        }
        UiIcon::Send => {
            painter.line_segment([point(0.0, 0.32), point(0.0, -0.31)], stroke);
            painter.line_segment([point(0.0, -0.31), point(-0.25, -0.07)], stroke);
            painter.line_segment([point(0.0, -0.31), point(0.25, -0.07)], stroke);
        }
        UiIcon::Settings => {
            painter.circle_stroke(center, size * 0.22, stroke);
            painter.circle_filled(center, size * 0.07, color);
            for step in 0..8 {
                let angle = std::f32::consts::TAU * step as f32 / 8.0;
                let direction = egui::vec2(angle.cos(), angle.sin());
                painter.line_segment(
                    [
                        center + direction * size * 0.29,
                        center + direction * size * 0.4,
                    ],
                    stroke,
                );
            }
        }
        UiIcon::Sort => {
            painter.line_segment([point(-0.3, -0.25), point(0.3, -0.25)], stroke);
            painter.line_segment([point(-0.2, 0.0), point(0.3, 0.0)], stroke);
            painter.line_segment([point(-0.08, 0.25), point(0.3, 0.25)], stroke);
        }
        UiIcon::Stop => {
            painter.rect_filled(
                egui::Rect::from_center_size(center, egui::vec2(size * 0.52, size * 0.52)),
                CornerRadius::same(1),
                color,
            );
        }
    }
}

fn brand_mark(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), size * 0.46, theme::ACCENT_SOFT);
    painter.circle_stroke(
        rect.center(),
        size * 0.34,
        Stroke::new((size * 0.08).max(1.4), theme::ACCENT),
    );
    painter.circle_filled(rect.center(), size * 0.09, theme::TEXT);
}

fn status_dot(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(size + 2.0, size + 2.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), size * 0.5, color);
}

fn draw_markdown(ui: &mut egui::Ui, cache: &mut CommonMarkCache, text: &str) {
    let max_width = ui.available_width().max(1.0) as usize;
    ui.scope(|ui| {
        ui.style_mut().url_in_tooltip = true;
        CommonMarkViewer::new()
            .max_image_width(Some(max_width))
            .default_width(Some(max_width))
            .show(ui, cache, text);
    });
}

fn model_display(models: &[ModelOption], id: &str) -> String {
    models
        .iter()
        .find(|m| m.id == id)
        .map(|m| m.display_name.clone())
        .unwrap_or_else(|| {
            if id.is_empty() {
                "Loading models…".into()
            } else {
                id.into()
            }
        })
}

fn selected_efforts(models: &[ModelOption], id: &str) -> Vec<String> {
    models
        .iter()
        .find(|m| m.id == id)
        .map(|m| m.efforts.clone())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()])
}

fn usage_window_ui(ui: &mut egui::Ui, window: &RateLimitWindow) {
    let remaining = 100_u32.saturating_sub(window.used_percent.min(100));
    ui.horizontal(|ui| {
        ui.label(usage_window_name(window.duration_minutes));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(format!("{remaining}% left")).strong());
        });
    });
    ui.add(
        egui::ProgressBar::new(remaining as f32 / 100.0)
            .desired_width(ui.available_width())
            .show_percentage(),
    );
    if let Some(resets_at) = window.resets_at {
        ui.label(
            RichText::new(format!("Resets {}", format_timestamp(resets_at)))
                .small()
                .color(theme::MUTED),
        );
    }
}

fn context_window_usage_ui(ui: &mut egui::Ui, usage: Option<ContextWindowUsage>) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(24.0, 30.0), egui::Sense::hover());
    let center = rect.center();
    let radius = 8.0;

    ui.painter()
        .circle_stroke(center, radius, Stroke::new(2.0_f32, theme::BORDER));

    let Some(usage) = usage else {
        response.on_hover_text("Context usage is available after the first response");
        return;
    };

    let ratio = (usage.used_tokens as f32 / usage.capacity_tokens as f32).clamp(0.0, 1.0);
    let percent = (ratio * 100.0).round() as u32;
    if ratio > 0.0 {
        let start_angle = -std::f32::consts::FRAC_PI_2;
        let sweep = std::f32::consts::TAU * ratio;
        let segment_count = (32.0 * ratio).ceil().max(2.0) as usize;
        let points = (0..=segment_count)
            .map(|index| {
                let angle = start_angle + sweep * index as f32 / segment_count as f32;
                center + egui::vec2(angle.cos(), angle.sin()) * radius
            })
            .collect();
        ui.painter().add(egui::Shape::line(
            points,
            Stroke::new(2.0_f32, theme::MUTED),
        ));
    }

    response.on_hover_text(format!(
        "{} of {} tokens used · {}% remaining",
        format_token_count(usage.used_tokens),
        format_token_count(usage.capacity_tokens),
        100_u32.saturating_sub(percent)
    ));
}

fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn usage_window_name(duration_minutes: Option<i64>) -> String {
    match duration_minutes {
        Some(10_080) => "Weekly limit".into(),
        Some(1_440) => "Daily limit".into(),
        Some(minutes) if minutes > 0 && minutes % 1_440 == 0 => {
            format!("{}-day limit", minutes / 1_440)
        }
        Some(minutes) if minutes > 0 && minutes % 60 == 0 => {
            format!("{}-hour limit", minutes / 60)
        }
        Some(minutes) if minutes > 0 => format!("{minutes}-minute limit"),
        _ => "Usage limit".into(),
    }
}

fn format_timestamp(timestamp: i64) -> String {
    local_system_time(timestamp)
        .map(|local| {
            const MONTHS: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let month = MONTHS[usize::from(local.wMonth - 1)];
            let (hour, meridiem) = match local.wHour {
                0 => (12, "AM"),
                1..=11 => (local.wHour, "AM"),
                12 => (12, "PM"),
                hour => (hour - 12, "PM"),
            };
            format!(
                "{month} {}, {} at {hour}:{:02} {meridiem}",
                local.wDay, local.wYear, local.wMinute
            )
        })
        .unwrap_or_else(|| format!("at Unix time {timestamp}"))
}

fn local_system_time(timestamp: i64) -> Option<SYSTEMTIME> {
    const WINDOWS_EPOCH_OFFSET_SECONDS: i128 = 11_644_473_600;
    const TICKS_PER_SECOND: i128 = 10_000_000;
    let ticks =
        (i128::from(timestamp) + WINDOWS_EPOCH_OFFSET_SECONDS).checked_mul(TICKS_PER_SECOND)?;
    let ticks = u64::try_from(ticks).ok()?;
    let file_time = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: all pointers reference initialized values for the duration of each call.
    if unsafe { FileTimeToSystemTime(&file_time, &mut utc) } == 0
        || unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0
        || !(1..=12).contains(&local.wMonth)
    {
        None
    } else {
        Some(local)
    }
}

fn sync_thread_messages(thread: &mut LocalThread, messages: &[ConversationItem]) {
    thread.messages.clear();
    thread.messages.extend_from_slice(messages);
}

fn apply_generated_thread_title(
    threads: &mut [LocalThread],
    local_thread_id: &str,
    title: String,
) -> bool {
    let Some(thread) = threads
        .iter_mut()
        .find(|thread| thread.id == local_thread_id && !thread.title_generated)
    else {
        return false;
    };
    thread.title = title;
    thread.title_generated = true;
    true
}

fn workspace_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
}

fn humanize(value: &str) -> String {
    let mut out = String::new();
    for (i, ch) in value.chars().enumerate() {
        if i > 0 && ch.is_uppercase() {
            out.push(' ');
        }
        if i == 0 {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(values) if values.iter().all(Value::is_string) => values
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn unix_timestamp_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn collect_files(root: &Path, dir: &Path, depth: usize, files: &mut Vec<String>) {
    if depth > 5 || files.len() >= 500 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if files.len() >= 500 {
            break;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || name == "target" || name == "node_modules" || name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, depth + 1, files);
        } else if let Ok(relative) = path.strip_prefix(root) {
            files.push(relative.to_string_lossy().to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_display_uses_friendly_name() {
        let models = vec![ModelOption {
            id: "gpt-test".into(),
            display_name: "GPT Test".into(),
            description: String::new(),
            efforts: vec![],
            default_effort: "high".into(),
            is_default: true,
        }];
        assert_eq!(model_display(&models, "gpt-test"), "GPT Test");
    }

    #[test]
    fn humanizes_protocol_item_names() {
        assert_eq!(humanize("fileChange"), "File Change");
    }

    #[test]
    fn cleans_model_generated_ui_summaries() {
        assert_eq!(
            clean_summary("**Inspected project structure.**\nExtra", 46),
            "Inspected project structure"
        );
    }

    #[test]
    fn recognizes_completed_activity_for_grouping() {
        let mut item = ConversationItem::new("command-1", ItemKind::Command, "cargo test");
        item.status = "completed".into();
        assert!(is_completed_activity(&item));
        item.status = "running".into();
        assert!(!is_completed_activity(&item));
    }

    #[test]
    fn fallback_activity_summaries_describe_the_work() {
        let mut first = ConversationItem::new(
            "command-1",
            ItemKind::Command,
            "rg --files; Get-Content src/app.rs",
        );
        first.status = "completed".into();
        let mut second = ConversationItem::new("command-2", ItemKind::Command, "Get-ChildItem src");
        second.status = "completed".into();
        assert_eq!(
            fallback_activity_summary(&[first, second]),
            "Inspected project files"
        );
    }

    #[test]
    fn labels_common_usage_windows() {
        assert_eq!(usage_window_name(Some(300)), "5-hour limit");
        assert_eq!(usage_window_name(Some(10_080)), "Weekly limit");
        assert_eq!(usage_window_name(Some(90)), "90-minute limit");
        assert_eq!(usage_window_name(None), "Usage limit");
    }

    #[test]
    fn formats_protocol_timestamps_for_people() {
        let formatted = format_timestamp(1_800_000_000);
        assert!(formatted.contains("2027"), "{formatted}");
        assert!(!formatted.contains("1800000000"), "{formatted}");
    }

    #[test]
    fn formats_context_window_token_counts() {
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(12_345), "12.3K");
        assert_eq!(format_token_count(1_500_000), "1.5M");
    }

    #[test]
    fn reads_context_usage_from_a_thread_notification() {
        let notification = json!({
            "threadId": "thread-1",
            "tokenUsage": {
                "last": {"totalTokens": 24_000},
                "total": {"totalTokens": 40_000},
                "modelContextWindow": 120_000
            }
        });
        assert_eq!(
            ContextWindowUsage::from_notification(&notification),
            Some(ContextWindowUsage {
                used_tokens: 24_000,
                capacity_tokens: 120_000
            })
        );
    }

    #[test]
    fn recognizes_only_the_skills_budget_warning() {
        assert!(is_skills_budget_warning(
            "Skill descriptions were shortened to fit the 2% skills context budget."
        ));
        assert!(!is_skills_budget_warning("Codex could not start the turn"));
    }

    #[test]
    fn syncing_streamed_messages_does_not_reorder_or_rename_threads() {
        let mut thread = test_thread("thread-a", "Alpha", 42);
        thread.title_generated = true;
        let mut message = ConversationItem::new("message-1", ItemKind::Assistant, "Codex");
        message.body = "streamed output".into();

        sync_thread_messages(&mut thread, &[message]);

        assert_eq!(thread.updated_at, 42);
        assert_eq!(thread.title, "Alpha");
        assert!(thread.title_generated);
        assert_eq!(thread.messages[0].body, "streamed output");
    }

    #[test]
    fn concurrent_title_results_stay_with_their_target_threads() {
        let mut threads = vec![
            test_thread("thread-a", "First request", 1),
            test_thread("thread-b", "Second request", 2),
        ];

        assert!(apply_generated_thread_title(
            &mut threads,
            "thread-b",
            "Beta title".into(),
        ));
        assert!(apply_generated_thread_title(
            &mut threads,
            "thread-a",
            "Alpha title".into(),
        ));
        assert!(!apply_generated_thread_title(
            &mut threads,
            "thread-a",
            "Stale override".into(),
        ));

        assert_eq!(threads[0].title, "Alpha title");
        assert_eq!(threads[1].title, "Beta title");
    }

    fn test_thread(id: &str, title: &str, updated_at: i64) -> LocalThread {
        LocalThread {
            id: id.into(),
            project_id: "project".into(),
            title: title.into(),
            created_at: 1,
            updated_at,
            title_generated: false,
            messages: Vec::new(),
            context_usage: None,
        }
    }
}
