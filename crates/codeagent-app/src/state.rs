use codeagent_core::{
    AccountInfo, AppHistory, Approval, ConversationItem, LocalThread, ModelOption, PersistedState,
    PlanUsage, Preferences, Project, ThreadAgentSettings,
};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<(String, String)>,
    pub answer: String,
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuestionRequest {
    pub request_id: Value,
    pub local_thread_id: Option<String>,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub message: String,
    pub is_error: bool,
}

pub struct AppState {
    pub connected: bool,
    pub connection_text: String,
    pub account: AccountInfo,
    pub plan_usage: Option<PlanUsage>,
    pub usage_loading: bool,
    pub reset_in_progress: bool,
    pub usage_error: Option<String>,
    pub models: Vec<ModelOption>,
    pub projects: Vec<Project>,
    pub threads: Vec<LocalThread>,
    pub active_project: Option<String>,
    pub active_local_thread: Option<String>,
    pub runtime_threads: HashMap<String, String>,
    pub running_turns: HashMap<String, Option<String>>,
    pub conversation: Vec<ConversationItem>,
    pub prefs: Preferences,
    pub approval: Option<Approval>,
    pub approval_queue: VecDeque<Approval>,
    pub user_question: Option<QuestionRequest>,
    pub user_question_queue: VecDeque<QuestionRequest>,
    pub toast: Option<Toast>,
    pub codex_update_version: Option<String>,
    pub codex_update_in_progress: bool,
    pub activity_log: Vec<String>,
    pub git_diff: String,
    pub files: Vec<String>,
    pub revision: u64,
}

impl AppState {
    pub fn from_persisted(persisted: PersistedState) -> Self {
        let mut state = Self {
            connected: false,
            connection_text: "Starting Codex…".into(),
            account: AccountInfo::default(),
            plan_usage: None,
            usage_loading: false,
            reset_in_progress: false,
            usage_error: None,
            models: Vec::new(),
            projects: Vec::new(),
            threads: Vec::new(),
            active_project: None,
            active_local_thread: None,
            runtime_threads: HashMap::new(),
            running_turns: HashMap::new(),
            conversation: Vec::new(),
            prefs: persisted.preferences,
            approval: None,
            approval_queue: VecDeque::new(),
            user_question: None,
            user_question_queue: VecDeque::new(),
            toast: None,
            codex_update_version: None,
            codex_update_in_progress: false,
            activity_log: vec!["CodeAgent launched".into()],
            git_diff: String::new(),
            files: Vec::new(),
            revision: 1,
        };
        state.apply_history(persisted.history);
        state
    }

    pub fn persisted(&mut self) -> PersistedState {
        self.sync_active_conversation();
        PersistedState {
            preferences: self.prefs.clone(),
            history: AppHistory {
                projects: self.projects.clone(),
                threads: self.threads.clone(),
                active_project: self.active_project.clone(),
                active_thread: self.active_local_thread.clone(),
            },
        }
    }

    pub fn apply_history(&mut self, mut history: AppHistory) {
        for thread in &mut history.threads {
            thread.agent.fill_missing_from(&self.prefs);
            for item in &mut thread.messages {
                if item.status != "running"
                    && matches!(
                        item.kind,
                        codeagent_core::ItemKind::Command
                            | codeagent_core::ItemKind::Tool
                            | codeagent_core::ItemKind::FileChange
                            | codeagent_core::ItemKind::Plan
                    )
                {
                    item.collapsed = true;
                }
            }
        }
        let active_project = history
            .active_project
            .filter(|id| history.projects.iter().any(|project| &project.id == id));
        let active_thread = history.active_thread.filter(|id| {
            history.threads.iter().any(|thread| {
                &thread.id == id && active_project.as_deref() == Some(&thread.project_id)
            })
        });
        self.conversation = active_thread
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
        self.active_local_thread = active_thread;
        self.touch();
    }

    pub fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn active_project_path(&self) -> Option<&str> {
        let active = self.active_project.as_deref()?;
        self.projects
            .iter()
            .find(|project| project.id == active)
            .map(|project| project.path.as_str())
    }

    pub fn active_thread_busy(&self) -> bool {
        self.active_local_thread
            .as_ref()
            .is_some_and(|id| self.running_turns.contains_key(id))
    }

    pub fn active_agent(&self) -> ThreadAgentSettings {
        self.active_local_thread
            .as_deref()
            .and_then(|id| self.threads.iter().find(|thread| thread.id == id))
            .map(|thread| thread.agent.clone())
            .unwrap_or_else(|| ThreadAgentSettings::from_preferences(&self.prefs))
    }

    pub fn active_agent_mut(&mut self) -> Option<&mut ThreadAgentSettings> {
        let id = self.active_local_thread.as_deref()?;
        self.threads
            .iter_mut()
            .find(|thread| thread.id == id)
            .map(|thread| &mut thread.agent)
    }

    pub fn select_model(&mut self, id: &str, default_effort: &str, efforts: &[String]) {
        let Some(agent) = self.active_agent_mut() else {
            return;
        };
        let current_effort = agent.effort.clone();
        let effort = if efforts.is_empty() || efforts.contains(&current_effort) {
            current_effort
        } else {
            default_effort.to_owned()
        };

        agent.model = id.to_owned();
        agent.effort = effort;
        self.touch();
    }

    pub fn select_effort(&mut self, effort: &str) {
        if let Some(agent) = self.active_agent_mut() {
            agent.effort = effort.to_owned();
            self.touch();
        }
    }

    pub fn sync_active_conversation(&mut self) {
        if let Some(id) = self.active_local_thread.as_deref()
            && let Some(thread) = self.threads.iter_mut().find(|thread| thread.id == id)
        {
            thread.messages.clone_from(&self.conversation);
        }
    }

    pub fn open_thread(&mut self, id: &str) -> bool {
        if self.active_local_thread.as_deref() == Some(id) {
            return false;
        }
        self.sync_active_conversation();
        let Some(thread) = self.threads.iter().find(|thread| thread.id == id) else {
            return false;
        };
        let project_id = thread.project_id.clone();
        let messages = thread.messages.clone();
        let Some(path) = self
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .map(|project| project.path.clone())
        else {
            return false;
        };
        self.active_project = Some(project_id);
        self.active_local_thread = Some(id.to_owned());
        self.prefs.workspace = path;
        self.conversation = messages;
        self.activity_log.push("Opened local conversation".into());
        self.touch();
        true
    }

    pub fn select_project(&mut self, id: &str) -> bool {
        let Some(path) = self
            .projects
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.path.clone())
        else {
            return false;
        };
        self.sync_active_conversation();
        self.active_project = Some(id.to_owned());
        self.prefs.workspace = path;
        self.active_local_thread = None;
        self.conversation.clear();
        self.touch();
        true
    }

    pub fn toggle_project(&mut self, id: &str) -> bool {
        let Some(project) = self.projects.iter_mut().find(|project| project.id == id) else {
            return false;
        };
        project.collapsed = !project.collapsed;
        self.touch();
        true
    }

    pub fn add_project(&mut self, path: String, now: i64) -> String {
        if let Some(existing) = self
            .projects
            .iter()
            .find(|project| project.path.eq_ignore_ascii_case(&path))
        {
            let id = existing.id.clone();
            self.select_project(&id);
            return id;
        }
        let id = format!("project-{now}-{}", self.projects.len() + 1);
        let name = std::path::Path::new(&path)
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or(&path)
            .to_owned();
        self.projects.push(Project {
            id: id.clone(),
            name,
            path: path.clone(),
            created_at: now,
            collapsed: false,
        });
        self.active_project = Some(id.clone());
        self.active_local_thread = None;
        self.prefs.workspace = path;
        self.conversation.clear();
        self.activity_log.push("Project added".into());
        self.touch();
        id
    }

    pub fn new_thread(&mut self, now: i64) -> Option<String> {
        let project_id = self.active_project.clone()?;
        self.sync_active_conversation();
        let id = format!("thread-{now}-{}", self.threads.len() + 1);
        self.threads.push(LocalThread {
            id: id.clone(),
            project_id,
            title: "New thread".into(),
            created_at: now,
            updated_at: now,
            title_generated: false,
            messages: Vec::new(),
            context_usage: None,
            agent: ThreadAgentSettings::from_preferences(&self.prefs),
        });
        self.active_local_thread = Some(id.clone());
        self.conversation.clear();
        self.activity_log.push("New conversation".into());
        self.touch();
        Some(id)
    }

    pub fn archive_thread(&mut self, id: &str) -> bool {
        if self.running_turns.contains_key(id) {
            self.info("Stop this thread before archiving it");
            return false;
        }
        let old_len = self.threads.len();
        self.threads.retain(|thread| thread.id != id);
        self.runtime_threads.remove(id);
        if self.active_local_thread.as_deref() == Some(id) {
            self.active_local_thread = None;
            self.conversation.clear();
        }
        let changed = old_len != self.threads.len();
        if changed {
            self.touch();
        }
        changed
    }

    pub fn enqueue_approval(&mut self, approval: Approval) {
        if self.approval.is_none() {
            self.approval = Some(approval);
        } else {
            self.approval_queue.push_back(approval);
        }
        self.touch();
    }

    pub fn enqueue_question(&mut self, question: QuestionRequest) {
        if self.user_question.is_none() {
            self.user_question = Some(question);
        } else {
            self.user_question_queue.push_back(question);
        }
        self.touch();
    }

    pub fn error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.activity_log.push(format!("Error: {message}"));
        self.toast = Some(Toast {
            message,
            is_error: true,
        });
        self.touch();
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.toast = Some(Toast {
            message: message.into(),
            is_error: false,
        });
        self.touch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeagent_core::{ItemKind, PersistedState};

    fn state() -> AppState {
        AppState::from_persisted(PersistedState::default())
    }

    #[test]
    fn invalid_active_ids_are_discarded() {
        let mut state = state();
        state.apply_history(AppHistory {
            projects: vec![],
            threads: vec![],
            active_project: Some("missing".into()),
            active_thread: Some("missing".into()),
        });
        assert!(state.active_project.is_none());
        assert!(state.active_local_thread.is_none());
    }

    #[test]
    fn project_thread_and_conversation_lifecycle_is_stable() {
        let mut state = state();
        let project = state.add_project(r"C:\code\demo".into(), 10);
        let thread = state.new_thread(20).unwrap();
        state
            .conversation
            .push(ConversationItem::new("m", ItemKind::Assistant, "Codex"));
        state.sync_active_conversation();
        assert_eq!(state.threads[0].messages.len(), 1);
        assert_eq!(state.active_project.as_deref(), Some(project.as_str()));
        assert!(state.archive_thread(&thread));
        assert!(state.active_local_thread.is_none());
    }

    #[test]
    fn duplicate_project_path_selects_existing_project() {
        let mut state = state();
        let first = state.add_project(r"C:\Code\Demo".into(), 1);
        let second = state.add_project(r"c:\code\demo".into(), 2);
        assert_eq!(first, second);
        assert_eq!(state.projects.len(), 1);
    }

    #[test]
    fn project_headers_toggle_their_thread_visibility_state() {
        let mut state = state();
        let project = state.add_project(r"C:\Code\Demo".into(), 1);
        assert!(!state.projects[0].collapsed);
        assert!(state.toggle_project(&project));
        assert!(state.projects[0].collapsed);
        assert!(state.toggle_project(&project));
        assert!(!state.projects[0].collapsed);
    }

    #[test]
    fn opening_sidebar_thread_switches_to_its_project_and_conversation() {
        let mut state = state();
        let first_project = state.add_project(r"C:\Code\First".into(), 1);
        let first_thread = state.new_thread(2).unwrap();
        state
            .conversation
            .push(ConversationItem::new("saved", ItemKind::Assistant, "Codex"));
        state.sync_active_conversation();

        state.add_project(r"C:\Code\Second".into(), 3);
        state.new_thread(4).unwrap();

        assert!(state.open_thread(&first_thread));
        assert_eq!(
            state.active_project.as_deref(),
            Some(first_project.as_str())
        );
        assert_eq!(
            state.active_local_thread.as_deref(),
            Some(first_thread.as_str())
        );
        assert_eq!(state.prefs.workspace, r"C:\Code\First");
        assert_eq!(state.conversation.len(), 1);
        assert_eq!(state.conversation[0].id, "saved");
    }

    #[test]
    fn busy_sidebar_thread_cannot_be_archived() {
        let mut state = state();
        state.add_project(r"C:\Code\Demo".into(), 1);
        let thread = state.new_thread(2).unwrap();
        state.running_turns.insert(thread.clone(), None);

        assert!(!state.archive_thread(&thread));
        assert!(state.threads.iter().any(|candidate| candidate.id == thread));
        assert_eq!(
            state.toast.as_ref().map(|toast| toast.message.as_str()),
            Some("Stop this thread before archiving it")
        );
    }

    #[test]
    fn new_threads_copy_defaults_and_keep_independent_agent_settings() {
        let mut state = state();
        state.prefs.model = "gpt-5.6-sol".into();
        state.prefs.effort = "high".into();
        state.prefs.sandbox = codeagent_core::SandboxChoice::FullAccess;
        state.add_project(r"C:\Code\Demo".into(), 1);

        let first = state.new_thread(2).unwrap();
        state.prefs.model = "gpt-5.6-luna".into();
        state.prefs.effort = "low".into();
        state.prefs.sandbox = codeagent_core::SandboxChoice::ReadOnly;
        let second = state.new_thread(3).unwrap();

        let first_agent = &state
            .threads
            .iter()
            .find(|thread| thread.id == first)
            .unwrap()
            .agent;
        assert_eq!(first_agent.model, "gpt-5.6-sol");
        assert_eq!(first_agent.effort, "high");
        assert_eq!(
            first_agent.sandbox,
            Some(codeagent_core::SandboxChoice::FullAccess)
        );

        assert_eq!(state.active_local_thread.as_deref(), Some(second.as_str()));
        assert_eq!(state.active_agent().model, "gpt-5.6-luna");
        assert_eq!(state.active_agent().effort, "low");
    }

    #[test]
    fn composer_changes_without_an_active_thread_do_not_replace_defaults() {
        let mut state = state();
        state.prefs.model = "gpt-5.6-sol".into();
        state.prefs.effort = "high".into();
        let efforts = vec!["low".into(), "high".into()];

        state.select_model("gpt-5.6-terra", "low", &efforts);
        state.select_effort("low");

        assert_eq!(state.prefs.model, "gpt-5.6-sol");
        assert_eq!(state.prefs.effort, "high");
    }

    #[test]
    fn composer_model_and_effort_are_restored_per_thread() {
        let mut state = state();
        state.prefs.model = "gpt-5.6-sol".into();
        state.prefs.effort = "high".into();
        state.add_project(r"C:\Code\Demo".into(), 1);
        let first = state.new_thread(2).unwrap();
        let efforts = vec!["low".into(), "xhigh".into()];

        state.select_model("gpt-5.6-terra", "low", &efforts);
        state.select_effort("xhigh");
        let second = state.new_thread(3).unwrap();

        let first_agent = &state
            .threads
            .iter()
            .find(|thread| thread.id == first)
            .unwrap()
            .agent;
        assert_eq!(first_agent.model, "gpt-5.6-terra");
        assert_eq!(first_agent.effort, "xhigh");

        let second_agent = &state
            .threads
            .iter()
            .find(|thread| thread.id == second)
            .unwrap()
            .agent;
        assert_eq!(second_agent.model, "gpt-5.6-sol");
        assert_eq!(second_agent.effort, "high");

        let persisted = state.persisted();
        let mut restored = AppState::from_persisted(persisted);
        assert_eq!(
            restored.active_local_thread.as_deref(),
            Some(second.as_str())
        );
        assert_eq!(restored.active_agent().model, "gpt-5.6-sol");
        assert_eq!(restored.active_agent().effort, "high");

        assert!(restored.open_thread(&first));
        assert_eq!(restored.active_agent().model, "gpt-5.6-terra");
        assert_eq!(restored.active_agent().effort, "xhigh");
        assert_eq!(restored.prefs.model, "gpt-5.6-sol");
        assert_eq!(restored.prefs.effort, "high");
    }
}
