use crate::{AppState, Question, QuestionRequest, update, workspace};
use codeagent_core::{
    Approval, ContextWindowUsage, ConversationItem, ItemKind, PersistedState, PlanUsage,
    truncate_text,
};
use codeagent_protocol::CodexBackend;
use crossbeam_channel::{Receiver, unbounded};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

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
struct SummaryJob {
    key: String,
    local_thread_id: String,
    prompt: String,
    cwd: String,
    model: String,
    effort: String,
}

#[derive(Debug, Clone)]
struct ActiveSummary {
    key: String,
    local_thread_id: String,
    output: String,
}

pub struct Controller {
    pub state: AppState,
    backend: Option<CodexBackend>,
    backend_start: Option<Receiver<Result<CodexBackend, String>>>,
    workspace_rx: Option<Receiver<(String, Vec<String>)>>,
    update_check_rx: Option<Receiver<Result<Option<String>, String>>>,
    update_install_rx: Option<Receiver<Result<(), String>>>,
    pending: HashMap<u64, PendingCall>,
    summary_pending: HashSet<String>,
    active_summaries: HashMap<String, ActiveSummary>,
    next_id: u64,
}

impl Controller {
    pub fn new(persisted: PersistedState) -> Self {
        Self {
            state: AppState::from_persisted(persisted),
            backend: None,
            backend_start: None,
            workspace_rx: None,
            update_check_rx: None,
            update_install_rx: None,
            pending: HashMap::new(),
            summary_pending: HashSet::new(),
            active_summaries: HashMap::new(),
            next_id: 1,
        }
    }

    pub fn start(&mut self) {
        if self.backend_start.is_some() {
            return;
        }
        self.backend = None;
        self.pending.clear();
        self.summary_pending.clear();
        self.active_summaries.clear();
        self.state.connected = false;
        self.state.connection_text = "Starting Codex…".into();
        let (tx, rx) = unbounded();
        self.backend_start = Some(rx);
        let _ = thread::Builder::new()
            .name("codex-startup".into())
            .spawn(move || {
                let _ = tx.send(CodexBackend::spawn());
            });
        self.state.touch();
        self.refresh_workspace();
        self.check_for_codex_update();
    }

    pub fn poll(&mut self) -> bool {
        let previous = self.state.revision;
        if let Some(result) = self
            .backend_start
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.backend_start = None;
            self.attach_backend(result);
        }
        let messages = self
            .backend
            .as_ref()
            .map(|backend| {
                std::iter::from_fn(|| backend.try_recv())
                    .take(300)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for message in messages {
            self.handle_message(message);
        }
        if let Some((diff, files)) = self.workspace_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.workspace_rx = None;
            self.state.git_diff = diff;
            self.state.files = files;
            self.state.touch();
        }
        if let Some(result) = self
            .update_check_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.update_check_rx = None;
            match result {
                Ok(version) => {
                    self.state.codex_update_version = version;
                    self.state.touch();
                }
                Err(error) => {
                    self.state
                        .activity_log
                        .push(format!("Codex update check unavailable: {error}"));
                    self.state.touch();
                }
            }
        }
        if let Some(result) = self
            .update_install_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.update_install_rx = None;
            self.state.codex_update_in_progress = false;
            match result {
                Ok(()) => {
                    let version = self
                        .state
                        .codex_update_version
                        .take()
                        .unwrap_or_else(|| "latest version".into());
                    self.state
                        .activity_log
                        .push(format!("Updated Codex to {version}"));
                    self.state.info(format!(
                        "Codex {version} installed. Restart CodeAgent to use it."
                    ));
                }
                Err(error) => self.state.error(error),
            }
        }
        previous != self.state.revision
    }

    fn check_for_codex_update(&mut self) {
        if self.update_check_rx.is_some() || self.update_install_rx.is_some() {
            return;
        }
        let (tx, rx) = unbounded();
        self.update_check_rx = Some(rx);
        if let Err(error) = thread::Builder::new()
            .name("codex-update-check".into())
            .spawn(move || {
                let _ = tx.send(update::check());
            })
        {
            self.update_check_rx = None;
            self.state.activity_log.push(format!(
                "Codex update check unavailable: could not start worker: {error}"
            ));
            self.state.touch();
        }
    }

    pub fn update_codex(&mut self) {
        if self.state.codex_update_version.is_none()
            || self.state.codex_update_in_progress
            || self.update_install_rx.is_some()
        {
            return;
        }

        self.state.codex_update_in_progress = true;
        self.state.touch();
        let (tx, rx) = unbounded();
        self.update_install_rx = Some(rx);
        if let Err(error) = thread::Builder::new()
            .name("codex-update-install".into())
            .spawn(move || {
                let _ = tx.send(update::install());
            })
        {
            self.update_install_rx = None;
            self.state.codex_update_in_progress = false;
            self.state
                .error(format!("Could not start the Codex update: {error}"));
        }
    }

    pub fn add_project(&mut self, path: String) {
        self.state.add_project(path, unix_timestamp());
        self.state.new_thread(unix_timestamp());
        self.refresh_workspace();
    }

    pub fn select_project(&mut self, id: &str) {
        if self.state.select_project(id) {
            self.refresh_workspace();
        }
    }

    pub fn toggle_project(&mut self, id: &str) {
        if self.state.active_project.as_deref() != Some(id) {
            self.select_project(id);
        }
        self.state.toggle_project(id);
    }

    pub fn open_thread(&mut self, id: &str) {
        if self.state.open_thread(id) {
            self.refresh_workspace();
        }
    }

    pub fn new_thread(&mut self) {
        self.state.new_thread(unix_timestamp());
    }

    pub fn archive_thread(&mut self, id: &str) {
        self.state.archive_thread(id);
    }

    pub fn send_prompt(&mut self, text: String, attachments: Vec<String>) {
        let text = text.trim().to_owned();
        if text.is_empty()
            || self.state.active_thread_busy()
            || !self.state.connected
            || self.state.active_project.is_none()
        {
            return;
        }
        if self.state.active_local_thread.is_none() {
            self.state.new_thread(unix_timestamp());
        }
        let Some(local_thread_id) = self.state.active_local_thread.clone() else {
            return;
        };

        let restore_context = !self.state.runtime_threads.contains_key(&local_thread_id);
        let first_prompt = self.state.conversation.is_empty();
        let mut local = ConversationItem::new(
            format!("local-user-{}", self.next_id),
            ItemKind::User,
            "You",
        );
        local.body = text.clone();
        local.status = "completed".into();
        if !attachments.is_empty() {
            let names = attachments
                .iter()
                .map(|path| attachment_name(path))
                .collect::<Vec<_>>()
                .join(", ");
            local.body.push_str(&format!("\n\nAttached: {names}"));
        }
        self.state.conversation.push(local);
        if let Some(thread) = self
            .state
            .threads
            .iter_mut()
            .find(|thread| thread.id == local_thread_id)
        {
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
        self.state.sync_active_conversation();
        self.state
            .running_turns
            .insert(local_thread_id.clone(), None);
        self.state.touch();
        if first_prompt {
            self.start_title_summary(&local_thread_id, &text);
        }

        let mut turn_text = text;
        if restore_context {
            let transcript = conversation_context(&self.state.conversation);
            if !transcript.is_empty() {
                turn_text = format!(
                    "Continue this locally saved CodeAgent conversation. Use the transcript as context; do not repeat it in your answer.\n\n<conversation_history>\n{transcript}\n</conversation_history>\n\nCurrent user request:\n{turn_text}"
                );
            }
        }
        let mut input = vec![json!({"type":"text","text":turn_text,"text_elements":[]})];
        input.extend(attachments.into_iter().map(attachment_input));
        let agent = self.state.active_agent();
        let turn = PendingTurn {
            input,
            cwd: self.state.prefs.workspace.clone(),
            approval_policy: agent
                .approval
                .unwrap_or(self.state.prefs.approval)
                .wire()
                .to_owned(),
            sandbox: agent
                .sandbox
                .unwrap_or(self.state.prefs.sandbox)
                .wire()
                .to_owned(),
            model: agent.model,
            effort: agent.effort,
        };
        if let Some(runtime_thread_id) = self.state.runtime_threads.get(&local_thread_id).cloned() {
            self.start_turn(local_thread_id, runtime_thread_id, turn);
        } else {
            let mut params = json!({"cwd":turn.cwd,"ephemeral":true,"approvalPolicy":turn.approval_policy,"sandbox":turn.sandbox,"serviceName":"codeagent"});
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

    pub fn interrupt(&mut self) {
        let Some(local_id) = self.state.active_local_thread.clone() else {
            return;
        };
        if let (Some(thread_id), Some(turn_id)) = (
            self.state.runtime_threads.get(&local_id).cloned(),
            self.state.running_turns.get(&local_id).cloned().flatten(),
        ) {
            self.request(
                "turn/interrupt",
                json!({"threadId":thread_id,"turnId":turn_id}),
                PendingCall::Interrupt {
                    local_thread_id: local_id,
                },
            );
        }
    }

    pub fn answer_approval(&mut self, decision: &str) {
        let Some(approval) = self.state.approval.take() else {
            return;
        };
        let result = if matches!(
            approval.method.as_str(),
            "execCommandApproval" | "applyPatchApproval"
        ) {
            let decision = match decision {
                "acceptForSession" => json!("approved_for_session"),
                "accept" => json!("approved"),
                _ => json!({"denied":{"rejection":"Denied by user"}}),
            };
            json!({"decision":decision})
        } else {
            json!({"decision":decision})
        };
        self.send_raw(json!({"id":approval.request_id,"result":result}));
        self.state
            .activity_log
            .push(format!("Approval: {decision}"));
        self.state.approval = self.state.approval_queue.pop_front();
        self.state.touch();
    }

    pub fn set_question_answer(&mut self, index: usize, answer: String) {
        if let Some(question) = self
            .state
            .user_question
            .as_mut()
            .and_then(|request| request.questions.get_mut(index))
        {
            question.answer = answer;
            self.state.touch();
        }
    }

    pub fn submit_question_answers(&mut self) {
        let Some(request) = self.state.user_question.take() else {
            return;
        };
        let answers = request
            .questions
            .into_iter()
            .map(|question| (question.id, json!({"answers":[question.answer]})))
            .collect::<serde_json::Map<_, _>>();
        self.send_raw(json!({"id":request.request_id,"result":{"answers":answers}}));
        self.state.user_question = self.state.user_question_queue.pop_front();
        self.state
            .activity_log
            .push("Answered Codex question".into());
        self.state.touch();
    }

    pub fn refresh_plan_usage(&mut self) {
        if !self.state.connected || !self.state.account.authenticated {
            return;
        }
        self.state.usage_loading = true;
        self.state.usage_error = None;
        self.request(
            "account/rateLimits/read",
            Value::Null,
            PendingCall::RateLimits,
        );
    }

    pub fn consume_reset(&mut self) {
        if self.state.reset_in_progress {
            return;
        }
        self.state.reset_in_progress = true;
        let credit_id = self
            .state
            .plan_usage
            .as_ref()
            .and_then(|usage| usage.reset_credits.as_ref())
            .and_then(|credits| credits.iter().find(|credit| credit.status == "available"))
            .map(|credit| credit.id.clone());
        self.request(
            "account/rateLimitResetCredit/consume",
            json!({
                "creditId": credit_id,
                "idempotencyKey": format!("codeagent-{}-{}", unix_timestamp_millis(), self.next_id)
            }),
            PendingCall::ConsumeReset,
        );
        self.state.touch();
    }

    pub fn persisted(&mut self) -> PersistedState {
        self.state.persisted()
    }

    pub fn refresh_workspace(&mut self) {
        let Some(root) = self.state.active_project_path().map(str::to_owned) else {
            self.state.git_diff.clear();
            self.state.files.clear();
            self.workspace_rx = None;
            return;
        };
        if self.workspace_rx.is_some() {
            return;
        }
        let respect_gitignore = self.state.prefs.respect_gitignore;
        let (tx, rx) = unbounded();
        self.workspace_rx = Some(rx);
        let _ = thread::Builder::new()
            .name("workspace-inspector".into())
            .spawn(move || {
                let _ = tx.send(workspace::inspect(&root, respect_gitignore));
            });
    }

    pub fn restart_workspace_inspection(&mut self) {
        self.workspace_rx = None;
        self.refresh_workspace();
    }

    fn attach_backend(&mut self, result: Result<CodexBackend, String>) {
        match result {
            Ok(backend) => {
                self.backend = Some(backend);
                self.state.connection_text = "Connecting…".into();
                self.request("initialize", json!({"clientInfo":{"name":"codeagent","title":"CodeAgent","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true,"requestAttestation":false}}), PendingCall::Initialize);
            }
            Err(error) => {
                self.state.connection_text = "Codex unavailable".into();
                self.state.error(error);
            }
        }
        self.state.touch();
    }

    fn request(&mut self, method: &str, params: Value, kind: PendingCall) {
        let id = self.next_id;
        self.next_id += 1;
        if let Some(backend) = &self.backend {
            match backend.send(json!({"method":method,"id":id,"params":params})) {
                Ok(()) => {
                    self.pending.insert(id, kind);
                }
                Err(error) => self.state.error(error),
            }
        }
    }

    fn notify(&mut self, method: &str, params: Option<Value>) {
        let mut message = json!({"method":method});
        if let Some(params) = params {
            message["params"] = params;
        }
        self.send_raw(message);
    }

    fn send_raw(&mut self, value: Value) {
        if let Some(backend) = &self.backend
            && let Err(error) = backend.send(value)
        {
            self.state.error(error);
        }
    }

    fn handle_message(&mut self, message: Value) {
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            if message.get("method").is_some() {
                self.handle_server_request(message);
                return;
            }
            let pending = self.pending.remove(&id);
            if let Some(error) = message.get("error") {
                let text = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown Codex error")
                    .to_owned();
                let mut report_error = true;
                match &pending {
                    Some(
                        PendingCall::ThreadStart {
                            local_thread_id, ..
                        }
                        | PendingCall::TurnStart { local_thread_id },
                    ) => {
                        self.state.running_turns.remove(local_thread_id);
                    }
                    Some(PendingCall::SummaryThreadStart(job)) => {
                        self.summary_pending.remove(&job.key);
                        self.state
                            .activity_log
                            .push(format!("Title summary unavailable: {text}"));
                        report_error = false;
                    }
                    Some(PendingCall::SummaryTurnStart { thread_id }) => {
                        if let Some(active) = self.active_summaries.remove(thread_id) {
                            self.summary_pending.remove(&active.key);
                        }
                        self.state
                            .activity_log
                            .push(format!("Title summary unavailable: {text}"));
                        report_error = false;
                    }
                    _ => {}
                }
                if matches!(pending, Some(PendingCall::ConsumeReset)) {
                    self.state.reset_in_progress = false;
                }
                if report_error {
                    self.state.error(text);
                } else {
                    self.state.touch();
                }
            } else if let (Some(pending), Some(result)) = (pending, message.get("result")) {
                self.handle_response(pending, result.clone());
            }
            return;
        }
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            self.handle_notification(
                method,
                message.get("params").cloned().unwrap_or(Value::Null),
            );
        }
    }

    fn handle_response(&mut self, pending: PendingCall, result: Value) {
        match pending {
            PendingCall::Initialize => {
                self.state.connected = true;
                self.state.connection_text = "Codex connected".into();
                self.state.activity_log.push(format!(
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
                if self.state.account.authenticated {
                    self.refresh_plan_usage();
                }
            }
            PendingCall::RateLimits => {
                self.state.usage_loading = false;
                self.state.usage_error = None;
                let usage = PlanUsage::from_protocol(&result);
                if let Some(plan) = usage
                    .limits
                    .iter()
                    .find_map(|limit| (!limit.plan.is_empty()).then_some(limit.plan.clone()))
                {
                    self.state.account.plan = plan;
                }
                self.state.plan_usage = Some(usage);
            }
            PendingCall::ConsumeReset => {
                self.state.reset_in_progress = false;
                match result
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                {
                    "reset" => self.state.info("Codex plan usage was reset"),
                    "alreadyRedeemed" => self.state.info("This reset was already applied"),
                    "nothingToReset" => self
                        .state
                        .error("None of the current usage windows can be reset yet"),
                    "noCredit" => self.state.error("No usage resets are available"),
                    outcome => self
                        .state
                        .error(format!("Codex returned an unknown reset result: {outcome}")),
                }
                self.refresh_plan_usage();
            }
            PendingCall::Models => self.apply_models(&result),
            PendingCall::ThreadStart {
                local_thread_id,
                turn,
            } => {
                if let Some(thread_id) = result
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                {
                    self.state
                        .runtime_threads
                        .insert(local_thread_id.clone(), thread_id.clone());
                    self.start_turn(local_thread_id, thread_id, turn);
                } else {
                    self.state.running_turns.remove(&local_thread_id);
                    self.state
                        .error("Codex started a thread without returning its id");
                }
            }
            PendingCall::TurnStart { local_thread_id } => {
                let turn_id = result
                    .pointer("/turn/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.state.running_turns.insert(local_thread_id, turn_id);
            }
            PendingCall::Interrupt { local_thread_id } => {
                self.state.running_turns.remove(&local_thread_id);
                self.state.activity_log.push("Turn interrupted".into());
            }
            PendingCall::SummaryThreadStart(job) => {
                if let Some(thread_id) = result
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                {
                    self.active_summaries.insert(
                        thread_id.clone(),
                        ActiveSummary {
                            key: job.key,
                            local_thread_id: job.local_thread_id,
                            output: String::new(),
                        },
                    );
                    self.request(
                        "turn/start",
                        json!({
                            "threadId": thread_id,
                            "input": [{"type":"text","text":job.prompt,"text_elements":[]}],
                            "cwd": job.cwd,
                            "approvalPolicy": "never",
                            "sandbox": "read-only",
                            "model": job.model,
                            "effort": job.effort
                        }),
                        PendingCall::SummaryTurnStart { thread_id },
                    );
                } else {
                    self.summary_pending.remove(&job.key);
                    self.state
                        .activity_log
                        .push("Title summary did not return a thread id".into());
                }
            }
            PendingCall::SummaryTurnStart { .. } => {}
        }
        self.state.touch();
    }

    fn start_turn(&mut self, local_thread_id: String, thread_id: String, turn: PendingTurn) {
        self.request("turn/start", json!({"threadId":thread_id,"input":turn.input,"cwd":turn.cwd,"approvalPolicy":turn.approval_policy,"sandbox":turn.sandbox,"model":turn.model,"effort":turn.effort}), PendingCall::TurnStart { local_thread_id });
    }

    fn start_title_summary(&mut self, local_thread_id: &str, request: &str) {
        let key = format!("thread-title:{local_thread_id}");
        if !self.summary_pending.insert(key.clone()) {
            return;
        }
        self.request(
            "thread/start",
            json!({
                "cwd": self.state.prefs.workspace,
                "ephemeral": true,
                "approvalPolicy": "never",
                "sandbox": "read-only",
                "serviceName": "codeagent-summary",
                "model": self.state.prefs.summary_model
            }),
            PendingCall::SummaryThreadStart(SummaryJob {
                key,
                local_thread_id: local_thread_id.to_owned(),
                prompt: format!(
                    "Create a concise 3-7 word sidebar title for this request. Do not call tools. Return only the title, with no quotes, punctuation, or explanation.\n\nRequest:\n{}",
                    truncate_text(request, 2_000)
                ),
                cwd: self.state.prefs.workspace.clone(),
                model: self.state.prefs.summary_model.clone(),
                effort: self.state.prefs.summary_effort.clone(),
            }),
        );
    }

    fn apply_account(&mut self, result: &Value) {
        let account = result.get("account").filter(|value| !value.is_null());
        self.state.account.authenticated = account.is_some();
        self.state.account.label = account
            .and_then(|value| value.get("email").or_else(|| value.get("name")))
            .and_then(Value::as_str)
            .unwrap_or(if account.is_some() {
                "ChatGPT account"
            } else {
                "Not signed in"
            })
            .to_owned();
        self.state.account.plan = account
            .and_then(|value| value.get("planType"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
    }

    fn apply_models(&mut self, result: &Value) {
        self.state.models = result
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|model| {
                !model
                    .get("hidden")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .filter_map(|model| {
                let id = model
                    .get("model")
                    .or_else(|| model.get("id"))?
                    .as_str()?
                    .to_owned();
                let display_name = model
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned();
                let efforts = model
                    .get("supportedReasoningEfforts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|value| {
                        value
                            .get("reasoningEffort")
                            .or_else(|| value.get("effort"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect();
                Some(codeagent_core::ModelOption {
                    id,
                    display_name,
                    description: model
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    efforts,
                    default_effort: model
                        .get("defaultReasoningEffort")
                        .and_then(Value::as_str)
                        .unwrap_or("high")
                        .to_owned(),
                    is_default: model
                        .get("isDefault")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
            })
            .collect();
        if self.state.prefs.model.is_empty()
            && let Some(model) = self
                .state
                .models
                .iter()
                .find(|model| model.is_default)
                .or_else(|| self.state.models.first())
        {
            self.state.prefs.model.clone_from(&model.id);
            self.state.prefs.effort.clone_from(&model.default_effort);
        }

        if !self
            .state
            .models
            .iter()
            .any(|model| model.id == self.state.prefs.summary_model)
            && let Some(luna) = self.state.models.iter().find(|model| {
                model.display_name.to_ascii_lowercase().contains("5.6-luna")
                    || model.id.to_ascii_lowercase().contains("5.6-luna")
            })
        {
            self.state.prefs.summary_model.clone_from(&luna.id);
        }
        if let Some(summary_model) = self
            .state
            .models
            .iter()
            .find(|model| model.id == self.state.prefs.summary_model)
            && !summary_model.efforts.is_empty()
            && !summary_model
                .efforts
                .contains(&self.state.prefs.summary_effort)
        {
            self.state.prefs.summary_effort =
                if summary_model.efforts.iter().any(|effort| effort == "low") {
                    "low".into()
                } else {
                    summary_model.default_effort.clone()
                };
        }

        for thread in &mut self.state.threads {
            thread.agent.fill_missing_from(&self.state.prefs);
        }
    }

    fn handle_notification(&mut self, method: &str, params: Value) {
        if self.handle_summary_notification(method, &params) {
            return;
        }
        match method {
            "backend/exited" | "backend/protocolError" => {
                self.state.connected = false;
                self.state.connection_text = "Codex disconnected".into();
                self.state.running_turns.clear();
                self.state.error(
                    params
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or(method),
                );
            }
            "backend/stderr" => {
                if let Some(message) = params.get("message").and_then(Value::as_str)
                    && (message.contains("ERROR") || message.contains("WARN"))
                {
                    self.state.activity_log.push(format!("Codex: {message}"));
                    self.state.touch();
                }
            }
            "item/started" | "item/completed" => {
                if let (Some(local_id), Some(item)) =
                    (self.local_thread_id(&params), params.get("item"))
                {
                    self.ingest_item(&local_id, item, method == "item/completed");
                    self.state.touch();
                }
            }
            "item/agentMessage/delta" => {
                self.append_delta_for(&params, ItemKind::Assistant, "Codex")
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                self.append_delta_for(&params, ItemKind::Reasoning, "Reasoning")
            }
            "item/plan/delta" => self.append_delta_for(&params, ItemKind::Plan, "Plan"),
            "item/commandExecution/outputDelta" => {
                self.append_delta_for(&params, ItemKind::Command, "Command")
            }
            "turn/started" => {
                if let Some(local_id) = self.local_thread_id(&params) {
                    let turn_id = params
                        .pointer("/turn/id")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    self.state.running_turns.insert(local_id, turn_id);
                    self.state.activity_log.push("Agent turn started".into());
                    self.state.touch();
                }
            }
            "turn/completed" => {
                if let Some(local_id) = self.local_thread_id(&params) {
                    self.state.running_turns.remove(&local_id);
                }
                let status = params
                    .pointer("/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed");
                self.state.activity_log.push(format!("Turn {status}"));
                if status == "failed" {
                    self.state.error(
                        params
                            .pointer("/turn/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("The Codex turn failed"),
                    );
                }
                self.state.sync_active_conversation();
                self.refresh_workspace();
                self.state.touch();
            }
            "thread/tokenUsage/updated" => {
                if let (Some(local_id), Some(usage)) = (
                    self.local_thread_id(&params),
                    ContextWindowUsage::from_notification(&params),
                ) {
                    if let Some(thread) = self
                        .state
                        .threads
                        .iter_mut()
                        .find(|thread| thread.id == local_id)
                    {
                        thread.context_usage = Some(usage);
                    }
                    self.state.touch();
                }
            }
            "account/updated" => {
                if let Some(account) = params.get("account") {
                    self.apply_account(&json!({"account":account}));
                    self.state.touch();
                }
            }
            "account/rateLimits/updated" => self.refresh_plan_usage(),
            "warning" | "configWarning" | "error" => {
                let message = params
                    .get("message")
                    .or_else(|| params.get("summary"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex reported a warning");
                if !codeagent_core::is_skills_budget_warning(message) {
                    self.state.error(message);
                }
            }
            _ => {}
        }
    }

    fn handle_summary_notification(&mut self, method: &str, params: &Value) -> bool {
        let Some(thread_id) = params.get("threadId").and_then(Value::as_str) else {
            return false;
        };
        let Some(active) = self.active_summaries.get_mut(thread_id) else {
            return false;
        };

        match method {
            "item/agentMessage/delta" => {
                active.output.push_str(
                    params
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
            }
            "item/completed" => {
                if let Some(item) = params.get("item")
                    && item.get("type").and_then(Value::as_str) == Some("agentMessage")
                {
                    let text = string_field(item, "text");
                    if !text.is_empty() {
                        active.output = text;
                    }
                }
            }
            "turn/completed" => {
                let active = self
                    .active_summaries
                    .remove(thread_id)
                    .expect("checked above");
                self.summary_pending.remove(&active.key);
                let title = clean_summary(&active.output, 70);
                if !title.is_empty()
                    && let Some(thread) = self
                        .state
                        .threads
                        .iter_mut()
                        .find(|thread| thread.id == active.local_thread_id)
                {
                    thread.title = title;
                    thread.title_generated = true;
                }
                self.state.touch();
            }
            _ => {}
        }
        true
    }

    fn local_thread_id(&self, params: &Value) -> Option<String> {
        let runtime_id = params.get("threadId").and_then(Value::as_str)?;
        self.state
            .runtime_threads
            .iter()
            .find_map(|(local, runtime)| (runtime == runtime_id).then(|| local.clone()))
    }

    fn append_delta_for(&mut self, params: &Value, kind: ItemKind, title: &str) {
        if let Some(local_id) = self.local_thread_id(params) {
            self.append_delta(&local_id, params, kind, title);
            self.state.touch();
        }
    }

    fn messages_for_thread_mut(&mut self, local_id: &str) -> Option<&mut Vec<ConversationItem>> {
        if self.state.active_local_thread.as_deref() == Some(local_id) {
            Some(&mut self.state.conversation)
        } else {
            self.state
                .threads
                .iter_mut()
                .find(|thread| thread.id == local_id)
                .map(|thread| &mut thread.messages)
        }
    }

    fn append_delta(&mut self, local_id: &str, params: &Value, kind: ItemKind, title: &str) {
        let id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or("streaming")
            .to_owned();
        let delta = params
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(messages) = self.messages_for_thread_mut(local_id) else {
            return;
        };
        if let Some(item) = messages.iter_mut().find(|item| item.id == id) {
            item.body.push_str(delta);
        } else {
            let mut item = ConversationItem::new(id, kind, title);
            item.body.push_str(delta);
            messages.push(item);
        }
    }

    fn ingest_item(&mut self, local_id: &str, item: &Value, completed: bool) {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown-item")
            .to_owned();
        let kind_name = item.get("type").and_then(Value::as_str).unwrap_or("system");
        let (kind, title, body) = match kind_name {
            "userMessage" => (
                ItemKind::User,
                "You".into(),
                item.get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            "agentMessage" => (
                ItemKind::Assistant,
                "Codex".into(),
                string_field(item, "text"),
            ),
            "reasoning" => (
                ItemKind::Reasoning,
                "Reasoning".into(),
                ["summary", "content"]
                    .into_iter()
                    .flat_map(|field| {
                        item.get(field)
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                    })
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            "plan" => (ItemKind::Plan, "Plan".into(), string_field(item, "text")),
            "commandExecution" => (
                ItemKind::Command,
                item.get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("Command")
                    .into(),
                string_field(item, "aggregatedOutput"),
            ),
            "fileChange" => (
                ItemKind::FileChange,
                "File changes".into(),
                item.get("changes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|change| {
                        format!(
                            "{}: {}",
                            change
                                .get("kind")
                                .and_then(Value::as_str)
                                .unwrap_or("update"),
                            change.get("path").and_then(Value::as_str).unwrap_or("file")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            "mcpToolCall" | "dynamicToolCall" | "collabAgentToolCall" => (
                ItemKind::Tool,
                item.get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("Tool")
                    .into(),
                item.get("arguments").map(value_to_text).unwrap_or_default(),
            ),
            "webSearch" => (ItemKind::Tool, "Web search".into(), value_to_text(item)),
            other => (
                ItemKind::System,
                codeagent_core::humanize(other),
                value_to_text(item),
            ),
        };
        let Some(messages) = self.messages_for_thread_mut(local_id) else {
            return;
        };
        if kind == ItemKind::User
            && let Some(local) = messages
                .iter_mut()
                .rev()
                .find(|entry| entry.id.starts_with("local-user-"))
        {
            local.id = id;
            local.status = status(completed).into();
            return;
        }
        if let Some(existing) = messages.iter_mut().find(|entry| entry.id == id) {
            existing.kind = kind;
            existing.title = title;
            if !body.is_empty() && kind != ItemKind::User {
                existing.body = body;
            }
            existing.status = status(completed).into();
            existing.collapsed = completed
                && matches!(
                    kind,
                    ItemKind::Command | ItemKind::Tool | ItemKind::FileChange | ItemKind::Plan
                );
        } else {
            let mut entry = ConversationItem::new(id, kind, title);
            entry.body = body;
            entry.status = status(completed).into();
            entry.collapsed = completed
                && matches!(
                    kind,
                    ItemKind::Reasoning
                        | ItemKind::Command
                        | ItemKind::Tool
                        | ItemKind::FileChange
                        | ItemKind::Plan
                );
            messages.push(entry);
        }
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
                let command = params.get("command").map(value_to_text).unwrap_or_else(|| "Command requested".into());
                let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or_default();
                let reason = params.get("reason").and_then(Value::as_str).unwrap_or("Codex needs permission to run this command.");
                self.state.enqueue_approval(Approval { request_id, method, title: "Run command?".into(), detail: format!("{command}\n\nWorking directory: {cwd}\n{reason}"), allow_session: true });
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => {
                let reason = params.get("reason").and_then(Value::as_str).unwrap_or("Codex wants to edit files in this workspace.");
                let root = params.get("grantRoot").and_then(Value::as_str).unwrap_or(&self.state.prefs.workspace);
                self.state.enqueue_approval(Approval { request_id, method, title: "Apply file changes?".into(), detail: format!("{reason}\n\nTarget: {root}"), allow_session: true });
            }
            "item/tool/requestUserInput" => {
                let local_thread_id = self.local_thread_id(&params);
                let questions = params.get("questions").and_then(Value::as_array).into_iter().flatten().filter_map(|question| {
                    let id = question.get("id")?.as_str()?.to_owned();
                    let options = question.get("options").and_then(Value::as_array).into_iter().flatten().filter_map(|option| Some((option.get("label")?.as_str()?.to_owned(), option.get("description").and_then(Value::as_str).unwrap_or_default().to_owned()))).collect();
                    Some(Question { id, header: question.get("header").and_then(Value::as_str).unwrap_or("Question").to_owned(), question: question.get("question").and_then(Value::as_str).unwrap_or_default().to_owned(), options, answer: String::new(), secret: question.get("isSecret").and_then(Value::as_bool).unwrap_or(false) })
                }).collect::<Vec<_>>();
                self.state.enqueue_question(QuestionRequest { request_id, local_thread_id, questions });
            }
            _ => self.send_raw(json!({"id":request_id,"error":{"code":-32601,"message":format!("Unsupported request: {method}")}})),
        }
    }
}

fn status(completed: bool) -> &'static str {
    if completed { "completed" } else { "running" }
}

fn attachment_input(path: String) -> Value {
    let name = attachment_name(&path);
    if has_extension(
        &path,
        &[
            "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff",
        ],
    ) {
        json!({"type":"localImage","path":path})
    } else if has_extension(&path, &["mp3", "wav", "m4a", "ogg", "flac", "aac", "webm"]) {
        json!({"type":"localAudio","path":path})
    } else {
        json!({"type":"mention","name":name,"path":path})
    }
}

fn attachment_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_owned()
}

fn has_extension(path: &str, extensions: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}
fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn conversation_context(conversation: &[ConversationItem]) -> String {
    let mut parts = conversation
        .iter()
        .rev()
        .skip(1)
        .filter_map(|item| {
            let role = match item.kind {
                ItemKind::User => "User",
                ItemKind::Assistant => "Assistant",
                _ => return None,
            };
            (!item.body.trim().is_empty()).then(|| format!("{role}: {}", item.body.trim()))
        })
        .collect::<Vec<_>>();
    parts.reverse();
    let transcript = parts.join("\n\n");
    const MAX_CONTEXT_CHARS: usize = 60_000;
    if transcript.chars().count() <= MAX_CONTEXT_CHARS {
        transcript
    } else {
        transcript
            .chars()
            .rev()
            .take(MAX_CONTEXT_CHARS)
            .collect::<String>()
            .chars()
            .rev()
            .collect()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_delta_updates_active_conversation_without_backend() {
        let mut controller = Controller::new(PersistedState::default());
        let project = controller.state.add_project("demo".into(), 1);
        let local = controller.state.new_thread(2).unwrap();
        controller
            .state
            .runtime_threads
            .insert(local.clone(), "runtime".into());
        controller.handle_notification(
            "item/agentMessage/delta",
            json!({"threadId":"runtime","itemId":"agent-1","delta":"Hello"}),
        );
        controller.handle_notification(
            "item/agentMessage/delta",
            json!({"threadId":"runtime","itemId":"agent-1","delta":" world"}),
        );
        assert_eq!(controller.state.conversation[0].body, "Hello world");
        assert_eq!(
            controller.state.active_project.as_deref(),
            Some(project.as_str())
        );
    }

    #[test]
    fn completed_command_is_collapsed() {
        let mut controller = Controller::new(PersistedState::default());
        controller.state.add_project("demo".into(), 1);
        let local = controller.state.new_thread(2).unwrap();
        controller
            .state
            .runtime_threads
            .insert(local, "runtime".into());
        controller.handle_notification("item/completed", json!({"threadId":"runtime","item":{"id":"cmd","type":"commandExecution","command":"cargo test","aggregatedOutput":"ok"}}));
        assert!(controller.state.conversation[0].collapsed);
        assert_eq!(controller.state.conversation[0].status, "completed");
    }

    #[test]
    fn server_requests_queue_approvals() {
        let mut controller = Controller::new(PersistedState::default());
        controller.handle_message(json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"command":"cargo test","cwd":"demo"}}));
        assert_eq!(
            controller.state.approval.as_ref().unwrap().title,
            "Run command?"
        );
    }

    #[test]
    fn restored_context_excludes_the_current_request_and_non_chat_items() {
        let mut prior_user = ConversationItem::new("u1", ItemKind::User, "You");
        prior_user.body = "Earlier question".into();
        let mut tool = ConversationItem::new("tool", ItemKind::Tool, "Read");
        tool.body = "internal details".into();
        let mut answer = ConversationItem::new("a1", ItemKind::Assistant, "Codex");
        answer.body = "Earlier answer".into();
        let mut current = ConversationItem::new("u2", ItemKind::User, "You");
        current.body = "Current request".into();

        let context = conversation_context(&[prior_user, tool, answer, current]);
        assert_eq!(
            context,
            "User: Earlier question\n\nAssistant: Earlier answer"
        );
    }

    #[test]
    fn summary_titles_are_cleaned_for_sidebar_display() {
        assert_eq!(
            clean_summary("**Fix new-chat model defaults.**\nExtra", 70),
            "Fix new-chat model defaults"
        );
    }

    #[test]
    fn model_discovery_fills_new_thread_defaults_and_preserves_luna_summaries() {
        let mut controller = Controller::new(PersistedState::default());
        controller.state.add_project("demo".into(), 1);
        controller.state.new_thread(2);

        controller.apply_models(&json!({"data":[
            {
                "id":"gpt-5.6-sol",
                "displayName":"GPT-5.6-Sol",
                "isDefault":true,
                "defaultReasoningEffort":"high",
                "supportedReasoningEfforts":[
                    {"reasoningEffort":"low"},
                    {"reasoningEffort":"high"}
                ]
            },
            {
                "id":"gpt-5.6-luna",
                "displayName":"GPT-5.6-Luna",
                "defaultReasoningEffort":"low",
                "supportedReasoningEfforts":[{"reasoningEffort":"low"}]
            }
        ]}));

        assert_eq!(controller.state.prefs.model, "gpt-5.6-sol");
        assert_eq!(controller.state.active_agent().model, "gpt-5.6-sol");
        assert_eq!(controller.state.active_agent().effort, "high");
        assert_eq!(controller.state.prefs.summary_model, "gpt-5.6-luna");
        assert_eq!(controller.state.prefs.summary_effort, "low");
    }

    #[test]
    fn attachments_use_the_protocol_input_for_their_file_type() {
        assert_eq!(
            attachment_input(r"C:\tmp\screen.PNG".into()),
            json!({"type":"localImage","path":r"C:\tmp\screen.PNG"})
        );
        assert_eq!(
            attachment_input(r"C:\tmp\meeting.wav".into()),
            json!({"type":"localAudio","path":r"C:\tmp\meeting.wav"})
        );
        assert_eq!(
            attachment_input(r"C:\tmp\notes.pdf".into()),
            json!({"type":"mention","name":"notes.pdf","path":r"C:\tmp\notes.pdf"})
        );
    }
}
