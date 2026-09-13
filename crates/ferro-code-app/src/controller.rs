use crate::{AppState, Question, QuestionRequest, update, workspace};
use crossbeam_channel::{Receiver, unbounded};
use ferro_code_core::{
    Approval, ContextWindowUsage, ConversationItem, ItemKind, PersistedState, PlanUsage,
    truncate_text,
};
use ferro_code_protocol::{CodexBackend, Transport};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
        generation: u64,
    },
    Interrupt {
        local_thread_id: String,
        turn_id: String,
    },
    SummaryThreadStart(SummaryJob),
    SummaryTurnStart {
        thread_id: String,
    },
}

#[derive(Debug, Clone)]
struct PendingTurn {
    generation: u64,
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
    target: SummaryTarget,
    prompt: String,
    cwd: String,
    model: String,
    effort: String,
}

#[derive(Debug, Clone)]
struct ActiveSummary {
    key: String,
    target: SummaryTarget,
    output: String,
    deadline: Instant,
}

#[derive(Debug, Clone)]
enum SummaryTarget {
    ThreadTitle {
        local_thread_id: String,
    },
    GitCommit {
        root: String,
        snapshot: workspace::CommitSnapshot,
    },
}

pub struct Controller {
    pub state: AppState,
    backend: Option<Box<dyn Transport>>,
    backend_start: Option<Receiver<Result<CodexBackend, String>>>,
    startup_in_progress: bool,
    workspace_rx: Option<Receiver<(Option<String>, workspace::WorkspaceSnapshot)>>,
    workspace_snapshots: HashMap<String, workspace::WorkspaceSnapshot>,
    github_configuration: Option<bool>,
    git_action_rx: Option<Receiver<Result<String, String>>>,
    commit_prepare_rx: Option<Receiver<(String, Result<workspace::CommitSnapshot, String>)>>,
    update_check_rx: Option<Receiver<Result<Option<String>, String>>>,
    update_install_rx: Option<Receiver<Result<(), String>>>,
    pending: HashMap<u64, PendingCall>,
    summary_pending: HashSet<String>,
    active_summaries: HashMap<String, ActiveSummary>,
    next_id: u64,
    last_polled_revision: u64,
    turn_generations: HashMap<String, u64>,
    cancelled: HashSet<String>,
    completed_turns: HashSet<(String, String)>,
    deadlines: HashMap<u64, Instant>,
    startup_deadline: Option<Instant>,
}

impl Controller {
    pub fn new(persisted: PersistedState) -> Self {
        Self {
            state: AppState::from_persisted(persisted),
            backend: None,
            backend_start: None,
            startup_in_progress: false,
            workspace_rx: None,
            workspace_snapshots: HashMap::new(),
            github_configuration: None,
            git_action_rx: None,
            commit_prepare_rx: None,
            update_check_rx: None,
            update_install_rx: None,
            pending: HashMap::new(),
            summary_pending: HashSet::new(),
            active_summaries: HashMap::new(),
            next_id: 1,
            last_polled_revision: 0,
            turn_generations: HashMap::new(),
            cancelled: HashSet::new(),
            completed_turns: HashSet::new(),
            deadlines: HashMap::new(),
            startup_deadline: None,
        }
    }

    pub fn with_transport(persisted: PersistedState, transport: impl Transport + 'static) -> Self {
        let mut controller = Self::new(persisted);
        controller.backend = Some(Box::new(transport));
        controller
    }

    pub fn start(&mut self) {
        if self.backend_start.is_some() {
            return;
        }
        self.clear_backend_state();
        self.startup_deadline = Some(Instant::now() + Duration::from_secs(60));
        self.startup_in_progress = true;
        self.state.connected = false;
        self.state.connection_text = "Starting Codex…".into();
        let (tx, rx) = unbounded();
        self.backend_start = Some(rx);
        if let Err(error) = thread::Builder::new()
            .name("codex-startup".into())
            .spawn(move || {
                let _ = tx.send(CodexBackend::spawn());
            })
        {
            self.disconnect(&format!("Could not start Codex: {error}"));
        }
        self.state.touch();
        self.refresh_workspace();
    }

    pub fn poll(&mut self) -> bool {
        let previous = self.last_polled_revision;
        let now = Instant::now();
        if self
            .startup_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.disconnect("Codex startup timed out. Use Reconnect to retry.");
        }
        if self
            .active_summaries
            .values()
            .any(|summary| now >= summary.deadline)
        {
            self.disconnect(
                "Codex summary timed out. Reconnect to retry; staged files were retained.",
            );
        }
        let expired = self
            .deadlines
            .iter()
            .filter_map(|(id, deadline)| (now >= *deadline).then_some(*id))
            .collect::<Vec<_>>();
        for id in expired {
            // A timed-out start may still be executing remotely. Reconnect ends this
            // generation instead of enabling another turn over uncertain work.
            if matches!(
                self.pending.get(&id),
                Some(
                    PendingCall::ThreadStart { .. }
                        | PendingCall::TurnStart { .. }
                        | PendingCall::Interrupt { .. }
                        | PendingCall::SummaryThreadStart(_)
                        | PendingCall::SummaryTurnStart { .. }
                )
            ) {
                self.disconnect("Codex request timed out. Reconnect before retrying the task.");
                break;
            }
            self.handle_message(json!({"id":id,"error":{"message":"Codex request timed out"}}));
        }
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
        if let Some((root, snapshot)) = self.workspace_rx.as_ref().and_then(|rx| rx.try_recv().ok())
        {
            self.workspace_rx = None;
            self.github_configuration = Some(snapshot.git.github_configured);
            if let Some(root) = &root {
                self.workspace_snapshots
                    .insert(workspace_cache_key(root), snapshot.clone());
            }
            let active_root = self.state.active_project_path();
            let result_is_active = same_workspace(root.as_deref(), active_root);
            if result_is_active {
                self.apply_workspace_snapshot(snapshot);
            }
        }
        if let Some((root, result)) = self
            .commit_prepare_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.commit_prepare_rx = None;
            match result {
                Ok(snapshot) if self.state.connected => self.start_commit_summary(root, snapshot),
                Ok(_) => {
                    self.state.git_action_in_progress = false;
                    self.state.error(
                        "Codex disconnected while staging. Your staged changes are retained.",
                    );
                }
                Err(error) => {
                    self.state.git_action_in_progress = false;
                    self.state.error(error);
                }
            }
        }
        if let Some(result) = self
            .git_action_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.git_action_rx = None;
            self.state.git_action_in_progress = false;
            match result {
                Ok(message) => {
                    self.state.activity_log.push(message.clone());
                    self.state.info(message);
                }
                Err(error) => self.state.error(error),
            }
            self.invalidate_current_workspace_snapshot();
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
                        "Codex {version} installed. Restart Ferro Code to use it."
                    ));
                }
                Err(error) => self.state.error(error),
            }
        }
        if self.state.activity_log.len() > 1000 {
            let excess = self.state.activity_log.len() - 1000;
            self.state.activity_log.drain(..excess);
        }
        self.last_polled_revision = self.state.revision;
        previous != self.state.revision
    }

    /// Returns true until the initial Codex handshake and metadata requests
    /// have either completed or failed.
    pub fn startup_in_progress(&self) -> bool {
        self.startup_in_progress
    }

    fn finish_startup_if_ready(&mut self) {
        if self.startup_in_progress
            && !self.pending.values().any(|call| {
                matches!(
                    call,
                    PendingCall::Initialize
                        | PendingCall::Account
                        | PendingCall::Models
                        | PendingCall::RateLimits
                )
            })
        {
            self.startup_in_progress = false;
            self.startup_deadline = None;
        }
    }

    fn clear_backend_state(&mut self) {
        self.backend = None;
        self.backend_start = None;
        self.pending.clear();
        self.deadlines.clear();
        self.startup_deadline = None;
        self.turn_generations.clear();
        self.cancelled.clear();
        self.completed_turns.clear();
        self.state.runtime_threads.clear();
        let running = self.state.running_turns.keys().cloned().collect::<Vec<_>>();
        for id in running {
            self.state.interrupt_items(&id);
            self.state.finish_turn(&id, unix_timestamp_millis() as u64);
        }
        self.state.turn_started_at_ms.clear();
        self.state.approval = None;
        self.state.approval_queue.clear();
        self.state.user_question = None;
        self.state.user_question_queue.clear();
        self.state.usage_loading = false;
        self.state.reset_in_progress = false;
        if self.git_action_rx.is_none() && self.commit_prepare_rx.is_none() {
            self.state.git_action_in_progress = false;
        }
        self.summary_pending.clear();
        self.active_summaries.clear();
    }

    fn disconnect(&mut self, message: &str) {
        self.clear_backend_state();
        self.startup_in_progress = false;
        self.state.connected = false;
        self.state.connection_text = "Codex disconnected — Reconnect to retry".into();
        self.state.error(message);
    }

    fn finish_local_turn(&mut self, id: &str) {
        self.turn_generations.remove(id);
        self.cancelled.remove(id);
        if self
            .state
            .approval
            .as_ref()
            .is_some_and(|a| a.local_thread_id.as_deref() == Some(id))
        {
            self.state.approval = None;
        }
        self.state
            .approval_queue
            .retain(|a| a.local_thread_id.as_deref() != Some(id));
        if self.state.approval.is_none() {
            self.state.approval = self.state.approval_queue.pop_front();
        }
        if self
            .state
            .user_question
            .as_ref()
            .is_some_and(|q| q.local_thread_id.as_deref() == Some(id))
        {
            self.state.user_question = None;
        }
        self.state
            .user_question_queue
            .retain(|q| q.local_thread_id.as_deref() != Some(id));
        if self.state.user_question.is_none() {
            self.state.user_question = self.state.user_question_queue.pop_front();
        }
        self.state.finish_turn(id, unix_timestamp_millis() as u64);
        self.state.touch();
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

    pub fn restore_history(&mut self, persisted: PersistedState) {
        self.clear_backend_state();
        self.state = AppState::from_persisted(persisted);
        self.workspace_snapshots.clear();
        self.restart_workspace_inspection();
        self.start();
    }

    pub fn diagnostics(&self) -> Value {
        json!({"applicationVersion":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"architecture":std::env::consts::ARCH,
            "connection":self.state.connection_text,"connected":self.state.connected,"pendingRequests":self.pending.len(),
            "runningTurns":self.state.running_turns.len(),"projects":self.state.projects.len(),"threads":self.state.threads.len(),
            "archivedThreads":self.state.archived_threads.len(),"usageLoading":self.state.usage_loading,
            "note":"Conversation content, account identifiers, credentials, and command output are omitted."})
    }

    pub fn restore_last_prompt(&mut self) {
        if let Some(item) = self
            .state
            .conversation
            .iter()
            .rev()
            .find(|item| item.kind == ItemKind::User)
        {
            self.state.set_draft(ferro_code_core::Draft {
                text: item.body.clone(),
                attachments: item.attachments.clone(),
            });
        }
    }

    pub fn add_project(&mut self, path: String) {
        self.state.add_project(path, unix_timestamp());
        self.state.new_thread(unix_timestamp());
        self.switch_workspace_inspection();
    }

    pub fn select_project(&mut self, id: &str) {
        let previous_root = self.state.active_project_path().map(str::to_owned);
        if self.state.select_project(id) {
            let current_root = self.state.active_project_path().map(str::to_owned);
            if !same_workspace(previous_root.as_deref(), current_root.as_deref()) {
                self.switch_workspace_inspection();
            }
        }
    }

    pub fn toggle_project(&mut self, id: &str) {
        if self.state.active_project.as_deref() != Some(id) {
            self.select_project(id);
        }
        self.state.toggle_project(id);
    }

    pub fn open_thread(&mut self, id: &str) {
        let previous_root = self.state.active_project_path().map(str::to_owned);
        if self.state.open_thread(id) {
            let current_root = self.state.active_project_path().map(str::to_owned);
            if !same_workspace(previous_root.as_deref(), current_root.as_deref()) {
                self.switch_workspace_inspection();
            }
        }
    }

    pub fn new_thread(&mut self) {
        self.state.new_thread(unix_timestamp());
    }

    pub fn new_thread_for_project(&mut self, id: &str) {
        if self.state.active_project.as_deref() != Some(id) {
            self.select_project(id);
        }
        if self.state.active_project.as_deref() == Some(id) {
            self.new_thread();
        }
    }

    pub fn archive_thread(&mut self, id: &str) {
        self.state.archive_thread(id);
    }

    pub fn toggle_message(&mut self, id: &str) {
        self.state.toggle_message(id);
    }

    pub fn toggle_activity_group(&mut self, id: &str) {
        self.state.toggle_activity_group(id);
    }

    pub fn toggle_response_details(&mut self, id: &str) {
        self.state.toggle_response_details(id);
    }

    pub fn send_prompt(&mut self, text: String, attachments: Vec<String>) -> bool {
        let text = text.trim().to_owned();
        if (text.is_empty() && attachments.is_empty())
            || self.state.active_thread_busy()
            || !self.state.connected
            || self.state.active_project.is_none()
        {
            return false;
        }
        if self.state.active_local_thread.is_none() {
            self.state.new_thread(unix_timestamp());
        }
        let Some(local_thread_id) = self.state.active_local_thread.clone() else {
            return false;
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
        local.attachments.clone_from(&attachments);
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
            .begin_turn(local_thread_id.clone(), unix_timestamp_millis() as u64);
        self.state.touch();
        let generation = self.next_id;
        self.turn_generations
            .insert(local_thread_id.clone(), generation);
        self.cancelled.remove(&local_thread_id);
        if first_prompt {
            self.start_title_summary(&local_thread_id, &text);
        }

        let mut turn_text = text;
        if restore_context {
            let transcript = conversation_context(&self.state.conversation);
            if !transcript.is_empty() {
                turn_text = format!(
                    "Continue this locally saved Ferro Code conversation. Use the transcript as context; do not repeat it in your answer.\n\n<conversation_history>\n{transcript}\n</conversation_history>\n\nCurrent user request:\n{turn_text}"
                );
            }
        }
        let mut input = vec![json!({"type":"text","text":turn_text,"text_elements":[]})];
        if restore_context {
            let mut historical = HashSet::new();
            for path in self
                .state
                .conversation
                .iter()
                .rev()
                .skip(1)
                .flat_map(|item| item.attachments.iter())
            {
                if historical.insert(path.clone()) {
                    if Path::new(path).is_file() {
                        input.push(attachment_input(path.clone()));
                    } else {
                        input.push(json!({"type":"text","text":format!("Previously attached file is unavailable: {path}"),"text_elements":[]}));
                    }
                }
            }
        }
        input.extend(attachments.into_iter().map(attachment_input));
        let agent = self.state.active_agent();
        let turn = PendingTurn {
            generation,
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
            let mut params = json!({"cwd":turn.cwd,"ephemeral":true,"approvalPolicy":turn.approval_policy,"sandbox":turn.sandbox,"serviceName":"ferro-code"});
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
        true
    }

    pub fn interrupt(&mut self) {
        let Some(local_id) = self.state.active_local_thread.clone() else {
            return;
        };
        if !self.state.running_turns.contains_key(&local_id) {
            return;
        }
        self.cancelled.insert(local_id.clone());
        self.interrupt_when_ready(&local_id);
    }

    fn interrupt_when_ready(&mut self, local_id: &str) {
        if let (Some(thread_id), Some(turn_id)) = (
            self.state.runtime_threads.get(local_id).cloned(),
            self.state.running_turns.get(local_id).cloned().flatten(),
        ) {
            if self.pending.values().any(|call| {
                matches!(call,
                PendingCall::Interrupt { local_thread_id, turn_id: pending_turn }
                if local_thread_id == local_id && pending_turn == &turn_id)
            }) {
                return;
            }
            self.request(
                "turn/interrupt",
                json!({"threadId":thread_id,"turnId":turn_id}),
                PendingCall::Interrupt {
                    local_thread_id: local_id.to_owned(),
                    turn_id,
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
        if !self.state.connected || !self.state.account.authenticated || self.state.usage_loading {
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
        if self.state.reset_in_progress || !self.state.connected {
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
                "idempotencyKey": format!("ferro-code-{}-{}", unix_timestamp_millis(), self.next_id)
            }),
            PendingCall::ConsumeReset,
        );
        self.state.touch();
    }

    pub fn persisted(&mut self) -> PersistedState {
        self.state.persisted()
    }

    pub fn refresh_workspace(&mut self) {
        let root = self.state.active_project_path().map(str::to_owned);
        if self.workspace_rx.is_some() {
            return;
        }
        let respect_gitignore = self.state.prefs.respect_gitignore;
        let github_configuration = self.github_configuration;
        let (tx, rx) = unbounded();
        self.workspace_rx = Some(rx);
        let result_root = root.clone();
        if let Err(error) = thread::Builder::new()
            .name("workspace-inspector".into())
            .spawn(move || {
                let snapshot =
                    workspace::inspect(root.as_deref(), respect_gitignore, github_configuration);
                let _ = tx.send((result_root, snapshot));
            })
        {
            self.workspace_rx = None;
            self.state.workspace_loading = false;
            self.state
                .error(format!("Could not inspect the workspace: {error}"));
        }
    }

    pub fn restart_workspace_inspection(&mut self) {
        self.workspace_rx = None;
        self.github_configuration = None;
        self.refresh_workspace();
    }

    fn switch_workspace_inspection(&mut self) {
        self.workspace_rx = None;
        self.restore_cached_workspace();
        self.refresh_workspace();
    }

    fn invalidate_current_workspace_snapshot(&mut self) {
        if let Some(root) = self.state.active_project_path() {
            self.workspace_snapshots.remove(&workspace_cache_key(root));
        }
        self.switch_workspace_inspection();
    }

    fn restore_cached_workspace(&mut self) {
        let root = self.state.active_project_path().map(str::to_owned);
        let snapshot = root
            .as_deref()
            .and_then(|root| self.workspace_snapshots.get(&workspace_cache_key(root)))
            .cloned();
        if let Some(snapshot) = snapshot {
            self.apply_workspace_snapshot(snapshot);
            return;
        }

        let installed = self.state.git_status.installed;
        let github_configured = self
            .github_configuration
            .unwrap_or(self.state.git_status.github_configured);
        self.state.git_diff.clear();
        self.state.files.clear();
        self.state.git_status = workspace::GitStatus {
            installed,
            github_configured,
            ..workspace::GitStatus::default()
        };
        self.state.workspace_loading = root.is_some();
        self.state.touch();
    }

    fn apply_workspace_snapshot(&mut self, snapshot: workspace::WorkspaceSnapshot) {
        self.state.git_diff = snapshot.diff;
        self.state.files = snapshot.files;
        self.state.git_status = snapshot.git;
        self.state.workspace_loading = false;
        self.state.touch();
    }

    pub fn run_git_action(&mut self) {
        if self.state.git_action_in_progress {
            return;
        }
        let Some(root) = self.state.active_project_path().map(str::to_owned) else {
            self.state.error("Select a project before using Git");
            return;
        };
        let git = &self.state.git_status;
        if !git.installed {
            return;
        }
        if !git.is_repository {
            self.spawn_git_action(root, workspace::GitAction::Initialize);
        } else if git.has_changes {
            if !self.state.connected {
                self.state
                    .error("Codex must be connected to create the commit summary");
                return;
            }
            let (tx, rx) = unbounded();
            self.commit_prepare_rx = Some(rx);
            self.state.git_action_in_progress = true;
            self.state.touch();
            if let Err(error) =
                thread::Builder::new()
                    .name("commit-snapshot".into())
                    .spawn(move || {
                        let result = workspace::commit_context(&root);
                        let _ = tx.send((root, result));
                    })
            {
                self.commit_prepare_rx = None;
                self.state.git_action_in_progress = false;
                self.state.error(error.to_string());
            }
        } else if !git.has_github_remote {
            if !git.github_configured {
                self.state
                    .error("GitHub is not configured. Run `gh auth login` and try again.");
                return;
            }
            self.spawn_git_action(root, workspace::GitAction::Publish);
        } else if git.has_unpushed_commits {
            if !git.github_configured {
                self.state
                    .error("GitHub is not configured. Run `gh auth login` and try again.");
                return;
            }
            self.spawn_git_action(root, workspace::GitAction::Push);
        } else {
            self.state.info("Repository is up to date");
        }
    }

    fn spawn_git_action(&mut self, root: String, action: workspace::GitAction) {
        self.state.git_action_in_progress = true;
        self.state.touch();
        let private_repository = self.state.prefs.github_private_repositories;
        let (tx, rx) = unbounded();
        self.git_action_rx = Some(rx);
        if let Err(error) = thread::Builder::new()
            .name("git-action".into())
            .spawn(move || {
                let _ = tx.send(workspace::run_action(&root, action, private_repository));
            })
        {
            self.git_action_rx = None;
            self.state.git_action_in_progress = false;
            self.state
                .error(format!("Could not start the Git action: {error}"));
        }
    }

    fn attach_backend(&mut self, result: Result<CodexBackend, String>) {
        self.startup_deadline = None;
        match result {
            Ok(backend) => {
                let uses_cli_fallback = backend.uses_cli_fallback();
                self.backend = Some(Box::new(backend));
                if uses_cli_fallback {
                    self.check_for_codex_update();
                }
                self.state.connection_text = "Connecting…".into();
                self.request("initialize", json!({"clientInfo":{"name":"ferro-code","title":"Ferro Code","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true,"requestAttestation":false}}), PendingCall::Initialize);
            }
            Err(error) => {
                self.startup_in_progress = false;
                self.state.connection_text = "Codex unavailable".into();
                self.state.error(error);
            }
        }
        self.state.touch();
    }

    fn request(&mut self, method: &str, params: Value, kind: PendingCall) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, kind);
        self.deadlines
            .insert(id, Instant::now() + Duration::from_secs(60));
        let params = if method == "turn/start" {
            ferro_code_protocol::turn_params(params)
        } else {
            Ok(params)
        };
        let result = params.and_then(|params| {
            self.backend
                .as_ref()
                .ok_or_else(|| "Codex is disconnected".to_owned())?
                .send(json!({"method":method,"id":id,"params":params}))
        });
        if let Err(error) = result {
            self.handle_message(json!({"id":id,"error":{"message":error}}));
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
        if message.get("id").is_some() && message.get("method").is_some() {
            self.handle_server_request(message);
            return;
        }
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            self.deadlines.remove(&id);
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
                        | PendingCall::TurnStart {
                            local_thread_id, ..
                        },
                    ) => {
                        // Only the matching generation can finish this local turn.
                        let generation = match pending.as_ref() {
                            Some(PendingCall::ThreadStart { turn, .. }) => turn.generation,
                            Some(PendingCall::TurnStart { generation, .. }) => *generation,
                            _ => 0,
                        };
                        if self.turn_generations.get(local_thread_id) == Some(&generation) {
                            self.finish_local_turn(local_thread_id);
                        }
                    }
                    Some(PendingCall::SummaryThreadStart(job)) => {
                        self.summary_pending.remove(&job.key);
                        if matches!(job.target, SummaryTarget::GitCommit { .. }) {
                            self.state.git_action_in_progress = false;
                        } else {
                            self.state
                                .activity_log
                                .push(format!("Title summary unavailable: {text}"));
                            report_error = false;
                        }
                    }
                    Some(PendingCall::SummaryTurnStart { thread_id }) => {
                        if let Some(active) = self.active_summaries.remove(thread_id) {
                            self.summary_pending.remove(&active.key);
                            if matches!(active.target, SummaryTarget::GitCommit { .. }) {
                                self.state.git_action_in_progress = false;
                            } else {
                                self.state
                                    .activity_log
                                    .push(format!("Title summary unavailable: {text}"));
                                report_error = false;
                            }
                        } else {
                            report_error = false;
                        }
                    }
                    _ => {}
                }
                if matches!(&pending, Some(PendingCall::RateLimits)) {
                    self.state.usage_loading = false;
                    self.state.usage_error = Some(text.clone());
                }
                if matches!(&pending, Some(PendingCall::Initialize)) {
                    self.state.connected = false;
                    self.state.connection_text = "Connection failed — Reconnect to retry".into();
                }
                if matches!(&pending, Some(PendingCall::ConsumeReset)) {
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
            self.finish_startup_if_ready();
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
                self.state.connection_text = format!(
                    "Connected: {}",
                    result
                        .get("userAgent")
                        .and_then(Value::as_str)
                        .unwrap_or("Codex")
                );
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
                if self.turn_generations.get(&local_thread_id) != Some(&turn.generation) {
                    return;
                }
                if self.cancelled.contains(&local_thread_id) {
                    self.finish_local_turn(&local_thread_id);
                    return;
                }
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
                    self.state
                        .finish_turn(&local_thread_id, unix_timestamp_millis() as u64);
                    self.state
                        .error("Codex started a thread without returning its id");
                }
            }
            PendingCall::TurnStart {
                local_thread_id,
                generation,
            } => {
                if self.turn_generations.get(&local_thread_id) != Some(&generation) {
                    return;
                }
                let turn_id = result
                    .pointer("/turn/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(turn_id) = turn_id {
                    if !self
                        .completed_turns
                        .contains(&(local_thread_id.clone(), turn_id.clone()))
                    {
                        self.state
                            .running_turns
                            .insert(local_thread_id.clone(), Some(turn_id));
                        if self.cancelled.contains(&local_thread_id) {
                            self.interrupt_when_ready(&local_thread_id);
                        }
                    }
                } else {
                    self.finish_local_turn(&local_thread_id);
                    self.state.error("Codex did not return a turn ID");
                }
            }
            PendingCall::Interrupt {
                local_thread_id,
                turn_id,
            } => {
                if self
                    .state
                    .running_turns
                    .get(&local_thread_id)
                    .and_then(|id| id.as_deref())
                    == Some(&turn_id)
                {
                    self.completed_turns
                        .insert((local_thread_id.clone(), turn_id));
                    self.state.interrupt_items(&local_thread_id);
                    self.finish_local_turn(&local_thread_id);
                    self.state.activity_log.push("Turn interrupted".into());
                }
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
                            target: job.target,
                            output: String::new(),
                            deadline: Instant::now() + Duration::from_secs(180),
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
                    if matches!(job.target, SummaryTarget::GitCommit { .. }) {
                        self.state.git_action_in_progress = false;
                        self.state
                            .error("Could not start the AI commit-message summary");
                    } else {
                        self.state
                            .activity_log
                            .push("Title summary did not return a thread id".into());
                    }
                }
            }
            PendingCall::SummaryTurnStart { .. } => {}
        }
        self.state.touch();
    }

    fn start_turn(&mut self, local_thread_id: String, thread_id: String, turn: PendingTurn) {
        self.request("turn/start", json!({"threadId":thread_id,"input":turn.input,"cwd":turn.cwd,"approvalPolicy":turn.approval_policy,"sandbox":turn.sandbox,"model":turn.model,"effort":turn.effort}), PendingCall::TurnStart { local_thread_id, generation: turn.generation });
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
                "serviceName": "ferro-code-summary",
                "model": self.state.prefs.summary_model
            }),
            PendingCall::SummaryThreadStart(SummaryJob {
                key,
                target: SummaryTarget::ThreadTitle {
                    local_thread_id: local_thread_id.to_owned(),
                },
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

    fn start_commit_summary(&mut self, root: String, snapshot: workspace::CommitSnapshot) {
        let key = format!("git-commit:{root}");
        if !self.summary_pending.insert(key.clone()) {
            return;
        }
        self.state.git_action_in_progress = true;
        self.state.touch();
        self.request(
            "thread/start",
            json!({
                "cwd": root,
                "ephemeral": true,
                "approvalPolicy": "never",
                "sandbox": "read-only",
                "serviceName": "ferro-code-summary",
                "model": self.state.prefs.summary_model
            }),
            PendingCall::SummaryThreadStart(SummaryJob {
                key,
                target: SummaryTarget::GitCommit { root: root.clone(), snapshot: snapshot.clone() },
                prompt: format!(
                    "Write a concise Git commit subject for these changes. Use imperative mood, stay under 72 characters, and return only the subject with no quotes or explanation. Do not call tools.\n\n{}",
                    truncate_text(&snapshot.context, 24_000)
                ),
                cwd: root,
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
                Some(ferro_code_core::ModelOption {
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
                self.disconnect(
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
                    if !self.state.running_turns.contains_key(&local_id)
                        || turn_id.as_ref().is_some_and(|id| {
                            self.completed_turns
                                .contains(&(local_id.clone(), id.clone()))
                        })
                    {
                        return;
                    }
                    self.state.running_turns.insert(local_id.clone(), turn_id);
                    if self.cancelled.contains(&local_id) {
                        self.interrupt_when_ready(&local_id);
                    }
                    self.state.activity_log.push("Agent turn started".into());
                    self.state.touch();
                }
            }
            "turn/completed" => {
                if let Some(local_id) = self.local_thread_id(&params) {
                    if let Some(turn_id) = params.pointer("/turn/id").and_then(Value::as_str) {
                        if !self
                            .completed_turns
                            .insert((local_id.clone(), turn_id.to_owned()))
                        {
                            return;
                        }
                        if self
                            .state
                            .running_turns
                            .get(&local_id)
                            .and_then(|id| id.as_deref())
                            .is_some_and(|id| id != turn_id)
                        {
                            return;
                        }
                    }
                    self.finish_local_turn(&local_id);
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
                if !ferro_code_core::is_skills_budget_warning(message) {
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
                match active.target {
                    SummaryTarget::ThreadTitle { local_thread_id } => {
                        let title = clean_summary(&active.output, 70);
                        if !title.is_empty()
                            && let Some(thread) = self
                                .state
                                .threads
                                .iter_mut()
                                .find(|thread| thread.id == local_thread_id)
                        {
                            thread.title = title;
                            thread.title_generated = true;
                        }
                    }
                    SummaryTarget::GitCommit { root, snapshot } => {
                        let status = params
                            .pointer("/turn/status")
                            .and_then(Value::as_str)
                            .unwrap_or("completed");
                        let message = clean_summary(&active.output, 72);
                        if status == "failed" {
                            self.state.git_action_in_progress = false;
                            self.state.error(
                                params
                                    .pointer("/turn/error/message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("The commit-message summary failed"),
                            );
                        } else if message.is_empty() {
                            self.state.git_action_in_progress = false;
                            self.state
                                .error("The summary model did not return a commit message");
                        } else {
                            self.spawn_git_commit(root, message, snapshot);
                        }
                    }
                }
                self.state.touch();
            }
            _ => {}
        }
        true
    }

    fn spawn_git_commit(
        &mut self,
        root: String,
        message: String,
        snapshot: workspace::CommitSnapshot,
    ) {
        let (tx, rx) = unbounded();
        self.git_action_rx = Some(rx);
        if let Err(error) = thread::Builder::new()
            .name("git-commit".into())
            .spawn(move || {
                let _ = tx.send(workspace::commit_snapshot(&root, &message, &snapshot));
            })
        {
            self.git_action_rx = None;
            self.state.git_action_in_progress = false;
            self.state
                .error(format!("Could not start the Git commit: {error}"));
        }
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
                ["arguments", "result", "error"]
                    .into_iter()
                    .filter_map(|field| {
                        item.get(field)
                            .filter(|value| !value.is_null())
                            .map(|value| format!("{field}: {}", value_to_text(value)))
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            ),
            "webSearch" => (ItemKind::Tool, "Web search".into(), value_to_text(item)),
            other => (
                ItemKind::System,
                ferro_code_core::humanize(other),
                value_to_text(item),
            ),
        };
        let item_status = if completed {
            match item.get("status").and_then(Value::as_str) {
                Some("failed" | "declined" | "interrupted") => item["status"].as_str().unwrap(),
                _ if item
                    .get("exitCode")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0)
                    || item.get("error").is_some_and(|error| !error.is_null()) =>
                {
                    "failed"
                }
                _ => "completed",
            }
        } else {
            "running"
        };
        let body = if let Some(code) = item.get("exitCode").and_then(Value::as_i64) {
            format!("{body}\nExit code: {code}")
        } else {
            body
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
            local.status = item_status.into();
            return;
        }
        if let Some(existing) = messages.iter_mut().find(|entry| entry.id == id) {
            existing.kind = kind;
            existing.title = title;
            if !body.is_empty() && kind != ItemKind::User {
                existing.body = body;
            }
            existing.status = item_status.into();
            if completed
                && matches!(
                    kind,
                    ItemKind::Command | ItemKind::Tool | ItemKind::FileChange | ItemKind::Plan
                )
            {
                existing.collapsed = true;
            }
        } else {
            let mut entry = ConversationItem::new(id, kind, title);
            entry.body = body;
            entry.status = item_status.into();
            entry.collapsed = completed
                && matches!(
                    kind,
                    ItemKind::Command | ItemKind::Tool | ItemKind::FileChange | ItemKind::Plan
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
        let local_thread_id = self.local_thread_id(&params);
        let origin = local_thread_id
            .as_ref()
            .and_then(|id| self.state.threads.iter().find(|t| &t.id == id))
            .map(|thread| {
                let project = self
                    .state
                    .projects
                    .iter()
                    .find(|p| p.id == thread.project_id);
                format!(
                    "{} — {}\n{}",
                    project
                        .map(|p| p.name.as_str())
                        .unwrap_or("Unknown project"),
                    thread.title,
                    project
                        .map(|p| p.path.as_str())
                        .unwrap_or("Unknown workspace")
                )
            })
            .unwrap_or_else(|| "Unknown requesting workspace".into());
        match method.as_str() {
            "item/commandExecution/requestApproval" | "execCommandApproval" => {
                let command = params.get("command").map(value_to_text).unwrap_or_else(|| "Command requested".into());
                let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or_default();
                let reason = params.get("reason").and_then(Value::as_str).unwrap_or("Codex needs permission to run this command.");
                self.state.enqueue_approval(Approval { local_thread_id, request_id, method, title: "Run command?".into(), detail: format!("{origin}\n\n{command}\n\nWorking directory: {cwd}\n{reason}"), allow_session: true });
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => {
                let reason = params.get("reason").and_then(Value::as_str).unwrap_or("Codex wants to edit files in this workspace.");
                let root = params.get("grantRoot").and_then(Value::as_str).unwrap_or("See requesting workspace above");
                self.state.enqueue_approval(Approval { local_thread_id, request_id, method, title: "Apply file changes?".into(), detail: format!("{origin}\n\n{reason}\n\nTarget: {root}"), allow_session: true });
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
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned()
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

fn same_workspace(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            #[cfg(windows)]
            {
                left.eq_ignore_ascii_case(right)
            }
            #[cfg(not(windows))]
            {
                left == right
            }
        }
        (None, None) => true,
        _ => false,
    }
}

fn workspace_cache_key(root: &str) -> String {
    #[cfg(windows)]
    {
        root.replace('/', "\\").to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        root.to_owned()
    }
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
    fn startup_waits_for_all_initial_codex_metadata() {
        let mut controller = Controller::new(PersistedState::default());
        controller.startup_in_progress = true;
        controller.pending.insert(1, PendingCall::Account);
        controller.pending.insert(2, PendingCall::Models);
        controller.pending.insert(3, PendingCall::RateLimits);

        controller.pending.remove(&1);
        controller.finish_startup_if_ready();
        assert!(controller.startup_in_progress());

        controller.pending.remove(&2);
        controller.finish_startup_if_ready();
        assert!(controller.startup_in_progress());

        controller.pending.remove(&3);
        controller.finish_startup_if_ready();
        assert!(!controller.startup_in_progress());
    }

    #[test]
    fn project_new_chat_switches_projects_before_creating_the_thread() {
        let mut controller = Controller::new(PersistedState::default());
        let first_project = controller.state.add_project("first".into(), 1);
        let second_project = controller.state.add_project("second".into(), 2);
        assert_eq!(
            controller.state.active_project.as_deref(),
            Some(second_project.as_str())
        );

        controller.new_thread_for_project(&first_project);

        assert_eq!(
            controller.state.active_project.as_deref(),
            Some(first_project.as_str())
        );
        assert_eq!(controller.state.threads.len(), 1);
        assert_eq!(controller.state.threads[0].project_id, first_project);
        assert_eq!(
            controller.state.active_local_thread.as_deref(),
            Some(controller.state.threads[0].id.as_str())
        );
    }

    #[test]
    fn project_switch_restores_cached_workspace_without_waiting_for_refresh() {
        let mut controller = Controller::new(PersistedState::default());
        let first_project = controller.state.add_project("first".into(), 1);
        let first_thread = controller.state.new_thread(2).unwrap();
        controller.state.add_project("second".into(), 3);
        controller.state.new_thread(4).unwrap();
        controller.github_configuration = Some(false);
        controller.workspace_snapshots.insert(
            workspace_cache_key("first"),
            workspace::WorkspaceSnapshot {
                diff: " M cached.txt".into(),
                files: vec!["cached.txt".into()],
                git: workspace::GitStatus {
                    installed: true,
                    is_repository: true,
                    has_changes: true,
                    ..workspace::GitStatus::default()
                },
            },
        );

        controller.open_thread(&first_thread);

        assert_eq!(
            controller.state.active_project.as_deref(),
            Some(first_project.as_str())
        );
        assert_eq!(controller.state.files, ["cached.txt"]);
        assert_eq!(controller.state.git_diff, " M cached.txt");
        assert!(!controller.state.workspace_loading);

        // Opening another thread in this project should not start another scan.
        controller.workspace_rx = None;
        let other_thread = controller.state.new_thread(5).unwrap();
        controller.open_thread(&first_thread);
        assert_eq!(
            controller.state.active_local_thread.as_deref(),
            Some(first_thread.as_str())
        );
        assert_ne!(other_thread, first_thread);
        assert!(controller.workspace_rx.is_none());
    }

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
    fn completed_command_collapses_while_response_details_remain_visible() {
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
    fn token_usage_notification_updates_the_matching_thread_immediately() {
        let mut controller = Controller::new(PersistedState::default());
        controller.state.add_project("demo".into(), 1);
        let local = controller.state.new_thread(2).unwrap();
        controller
            .state
            .runtime_threads
            .insert(local.clone(), "runtime".into());
        let previous_revision = controller.state.revision;

        controller.handle_notification(
            "thread/tokenUsage/updated",
            json!({
                "threadId": "runtime",
                "turnId": "turn-1",
                "tokenUsage": {
                    "last": {"totalTokens": 48_000},
                    "modelContextWindow": 120_000
                }
            }),
        );

        let usage = controller
            .state
            .threads
            .iter()
            .find(|thread| thread.id == local)
            .and_then(|thread| thread.context_usage)
            .unwrap();
        assert_eq!(usage.used_tokens, 48_000);
        assert_eq!(usage.capacity_tokens, 120_000);
        assert_eq!(usage.percent(), 40);
        assert!(controller.state.revision > previous_revision);
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

    #[test]
    fn sent_messages_retain_attachment_paths_for_the_ui() {
        let mut controller = Controller::new(PersistedState::default());
        controller.state.add_project("demo".into(), 1);
        controller.state.new_thread(2);
        controller.state.connected = true;

        controller.send_prompt("Describe this".into(), vec!["/tmp/screenshot.png".into()]);

        assert_eq!(
            controller.state.conversation[0].attachments,
            ["/tmp/screenshot.png"]
        );
        assert_eq!(controller.state.conversation[0].body, "Describe this");
    }
}

#[cfg(test)]
mod audit_controller {
    use super::*;
    #[test]
    fn string_request_ids_queue_approval() {
        let mut c = Controller::new(PersistedState::default());
        c.handle_message(json!({"id":"request-1", "method":"item/commandExecution/requestApproval", "params":{"command":"echo hi"}}));
        assert!(c.state.approval.is_some());
    }
    #[test]
    fn rate_limit_error_stops_loading() {
        let mut c = Controller::new(PersistedState::default());
        c.state.usage_loading = true;
        c.pending.insert(1, PendingCall::RateLimits);
        c.handle_message(json!({"id":1,"error":{"message":"test failure"}}));
        assert!(!c.state.usage_loading);
    }
    #[test]
    fn late_start_response_does_not_resurrect_completed_turn() {
        let mut c = Controller::new(PersistedState::default());
        c.state.add_project("project".into(), 1);
        let id = c.state.new_thread(2).unwrap();
        c.state.runtime_threads.insert(id.clone(), "runtime".into());
        c.state.begin_turn(id.clone(), 100);
        c.turn_generations.insert(id.clone(), 1);
        c.handle_notification(
            "turn/completed",
            json!({"threadId":"runtime","turn":{"id":"turn", "status":"completed"}}),
        );
        c.handle_response(
            PendingCall::TurnStart {
                local_thread_id: id,
                generation: 1,
            },
            json!({"turn":{"id":"turn"}}),
        );
        assert!(!c.state.active_thread_busy());
    }
    #[test]
    fn failed_commands_keep_failed_status() {
        let mut c = Controller::new(PersistedState::default());
        c.state.add_project("project".into(), 1);
        let id = c.state.new_thread(2).unwrap();
        c.ingest_item(&id, &json!({"id":"cmd", "type":"commandExecution", "command":"false", "status":"failed", "exitCode":1, "aggregatedOutput":""}), true);
        assert_eq!(c.state.conversation[0].status, "failed");
    }
    #[test]
    fn question_edits_are_detected_by_poll() {
        let mut c = Controller::new(PersistedState::default());
        c.handle_server_request(json!({"id":1,"method":"item/tool/requestUserInput","params":{"questions":[{"id":"q","question":"Pick one"}]}}));
        c.set_question_answer(0, "option".into());
        assert!(
            c.poll(),
            "UI timer relies on poll returning true to refresh question rows"
        );
    }
}
#[cfg(test)]
mod audit_approval_target {
    use super::*;
    #[test]
    fn background_file_approval_identifies_origin_project() {
        let mut c = Controller::new(PersistedState::default());
        c.state.add_project("project-a".into(), 1);
        let id = c.state.new_thread(2).unwrap();
        c.state.runtime_threads.insert(id, "runtime-a".into());
        c.state.add_project("project-b".into(), 3);
        c.handle_server_request(json!({"id":1,"method":"item/fileChange/requestApproval","params":{"threadId":"runtime-a"}}));
        assert!(
            c.state.approval.unwrap().detail.contains("project-a"),
            "Approval shows the foreground project instead of the requesting project"
        );
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    struct FakeTransport(Rc<RefCell<Vec<Value>>>);
    impl Transport for FakeTransport {
        fn send(&self, message: Value) -> Result<(), String> {
            self.0.borrow_mut().push(message);
            Ok(())
        }
        fn try_recv(&self) -> Option<Value> {
            None
        }
    }
    fn connected() -> (Controller, Rc<RefCell<Vec<Value>>>, String) {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut c =
            Controller::with_transport(PersistedState::default(), FakeTransport(sent.clone()));
        c.state.connected = true;
        c.state.add_project("project".into(), 1);
        let local = c.state.new_thread(2).unwrap();
        // Avoid a title-summary request in tests of the interactive turn.
        c.state
            .conversation
            .push(ConversationItem::new("previous", ItemKind::User, "User"));
        (c, sent, local)
    }
    fn request(sent: &Rc<RefCell<Vec<Value>>>, method: &str) -> Value {
        sent.borrow()
            .iter()
            .rev()
            .find(|v| v["method"] == method)
            .unwrap()
            .clone()
    }
    #[test]
    fn stop_before_thread_start_prevents_turn_request() {
        let (mut c, sent, _) = connected();
        assert!(c.send_prompt("hello".into(), vec![]));
        c.interrupt();
        let start = request(&sent, "thread/start");
        c.handle_message(json!({"id":start["id"],"result":{"thread":{"id":"runtime"}}}));
        assert!(!c.state.active_thread_busy());
        assert!(!sent.borrow().iter().any(|v| v["method"] == "turn/start"));
    }
    #[test]
    fn stop_during_turn_start_interrupts_once_id_arrives() {
        let (mut c, sent, local) = connected();
        c.state.runtime_threads.insert(local, "runtime".into());
        c.send_prompt("hello".into(), vec![]);
        let start = request(&sent, "turn/start");
        assert_eq!(start["params"]["sandboxPolicy"]["type"], "workspaceWrite");
        c.interrupt();
        c.handle_notification(
            "turn/started",
            json!({"threadId":"runtime","turn":{"id":"turn"}}),
        );
        c.handle_message(json!({"id":start["id"],"result":{"turn":{"id":"turn"}}}));
        assert_eq!(
            sent.borrow()
                .iter()
                .filter(|v| v["method"] == "turn/interrupt")
                .count(),
            1
        );
        assert_eq!(request(&sent, "turn/interrupt")["params"]["turnId"], "turn");
    }
    #[test]
    fn completed_turn_and_stale_interrupt_cannot_finish_next_turn() {
        let (mut c, sent, local) = connected();
        c.state
            .runtime_threads
            .insert(local.clone(), "runtime".into());
        c.send_prompt("first".into(), vec![]);
        let first = request(&sent, "turn/start");
        c.handle_notification(
            "turn/completed",
            json!({"threadId":"runtime","turn":{"id":"old"}}),
        );
        c.handle_message(json!({"id":first["id"],"result":{"turn":{"id":"old"}}}));
        assert!(!c.state.active_thread_busy());
        c.send_prompt("second".into(), vec![]);
        c.handle_notification(
            "turn/completed",
            json!({"threadId":"runtime","turn":{"id":"old"}}),
        );
        assert!(c.state.active_thread_busy());
        let second = request(&sent, "turn/start");
        c.handle_message(json!({"id":second["id"],"result":{"turn":{"id":"new"}}}));
        c.handle_response(
            PendingCall::Interrupt {
                local_thread_id: local,
                turn_id: "old".into(),
            },
            json!({}),
        );
        assert!(c.state.active_thread_busy());
    }
    #[test]
    fn disconnect_clears_runtime_queues_and_marks_partial_items_interrupted() {
        let (mut c, _, local) = connected();
        c.state.runtime_threads.insert(local, "runtime".into());
        c.send_prompt("hello".into(), vec![]);
        c.state
            .conversation
            .push(ConversationItem::new("command", ItemKind::Command, "work"));
        c.state.usage_loading = true;
        c.handle_server_request(json!({"id":"approval","method":"item/fileChange/requestApproval","params":{"threadId":"runtime"}}));
        c.handle_notification("backend/exited", json!({"message":"EOF"}));
        assert!(!c.state.connected);
        assert!(c.pending.is_empty());
        assert!(c.state.runtime_threads.is_empty());
        assert!(c.state.running_turns.is_empty());
        assert!(c.state.approval.is_none());
        assert!(!c.state.usage_loading);
        assert_eq!(c.state.conversation.last().unwrap().status, "interrupted");
    }
    #[test]
    fn expired_start_request_disconnects_instead_of_allowing_overlapping_retry() {
        let (mut c, _, _) = connected();
        c.send_prompt("hello".into(), vec![]);
        for deadline in c.deadlines.values_mut() {
            *deadline = Instant::now();
        }
        c.poll();
        assert!(!c.state.connected);
        assert!(!c.state.active_thread_busy());
    }
    #[test]
    fn restored_attachment_is_included_in_new_runtime_turn() {
        let (mut c, sent, _) = connected();
        let path = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        c.state.conversation[0].attachments.push(path.clone());
        c.send_prompt("use that file".into(), vec![]);
        let start = request(&sent, "thread/start");
        c.handle_message(json!({"id":start["id"],"result":{"thread":{"id":"runtime"}}}));
        let turn = request(&sent, "turn/start");
        assert!(
            turn["params"]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.to_string().contains(&path.replace('\\', "\\\\")))
        );
    }
}
