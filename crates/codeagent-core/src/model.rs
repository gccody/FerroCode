use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalThread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub title_generated: bool,
    #[serde(default)]
    pub messages: Vec<ConversationItem>,
    #[serde(default)]
    pub context_usage: Option<ContextWindowUsage>,
    #[serde(default)]
    pub agent: ThreadAgentSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadAgentSettings {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub sandbox: Option<SandboxChoice>,
    #[serde(default)]
    pub approval: Option<ApprovalChoice>,
}

impl ThreadAgentSettings {
    pub fn from_preferences(preferences: &Preferences) -> Self {
        Self {
            model: preferences.model.clone(),
            effort: preferences.effort.clone(),
            sandbox: Some(preferences.sandbox),
            approval: Some(preferences.approval),
        }
    }

    pub fn fill_missing_from(&mut self, preferences: &Preferences) {
        if self.model.is_empty() {
            self.model.clone_from(&preferences.model);
        }
        if self.effort.is_empty() {
            self.effort.clone_from(&preferences.effort);
        }
        self.sandbox.get_or_insert(preferences.sandbox);
        self.approval.get_or_insert(preferences.approval);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWindowUsage {
    pub used_tokens: u64,
    pub capacity_tokens: u64,
}

impl ContextWindowUsage {
    pub fn from_notification(params: &Value) -> Option<Self> {
        let used_tokens = params.pointer("/tokenUsage/last/totalTokens")?.as_u64()?;
        let capacity_tokens = params.pointer("/tokenUsage/modelContextWindow")?.as_u64()?;
        (capacity_tokens > 0).then_some(Self {
            used_tokens,
            capacity_tokens,
        })
    }

    pub fn percent(self) -> u32 {
        ((self.used_tokens.saturating_mul(100) / self.capacity_tokens.max(1)).min(100)) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    User,
    Assistant,
    Reasoning,
    Command,
    FileChange,
    Tool,
    Plan,
    System,
}

impl ItemKind {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Reasoning => "reasoning",
            Self::Command => "command",
            Self::FileChange => "file-change",
            Self::Tool => "tool",
            Self::Plan => "plan",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationItem {
    pub id: String,
    pub kind: ItemKind,
    pub title: String,
    pub body: String,
    pub status: String,
    pub collapsed: bool,
    #[serde(default)]
    pub summary: Option<String>,
}

impl ConversationItem {
    pub fn new(id: impl Into<String>, kind: ItemKind, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            title: title.into(),
            body: String::new(),
            status: "running".into(),
            collapsed: false,
            summary: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppHistory {
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub threads: Vec<LocalThread>,
    pub active_project: Option<String>,
    pub active_thread: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountInfo {
    pub label: String,
    pub plan: String,
    pub authenticated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanUsage {
    pub limits: Vec<RateLimit>,
    pub available_reset_count: u64,
    pub reset_credits: Option<Vec<ResetCredit>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimit {
    pub id: String,
    pub name: String,
    pub plan: String,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub credits_balance: Option<String>,
    pub credits_unlimited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitWindow {
    pub used_percent: u32,
    pub resets_at: Option<i64>,
    pub duration_minutes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetCredit {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub granted_at: i64,
    pub expires_at: Option<i64>,
}

impl PlanUsage {
    pub fn from_protocol(result: &Value) -> Self {
        let mut limits = result
            .get("rateLimitsByLimitId")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(id, value)| RateLimit::from_protocol(id, value))
            .collect::<Vec<_>>();
        limits.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        if limits.is_empty()
            && let Some(value) = result.get("rateLimits").filter(|value| !value.is_null())
        {
            let id = value
                .get("limitId")
                .and_then(Value::as_str)
                .unwrap_or("codex");
            limits.push(RateLimit::from_protocol(id, value));
        }

        let reset_summary = result.get("rateLimitResetCredits").filter(|v| !v.is_null());
        let available_reset_count = reset_summary
            .and_then(|v| v.get("availableCount"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let reset_credits = reset_summary
            .and_then(|v| v.get("credits"))
            .filter(|v| !v.is_null())
            .and_then(Value::as_array)
            .map(|credits| {
                credits
                    .iter()
                    .filter_map(ResetCredit::from_protocol)
                    .collect()
            });
        Self {
            limits,
            available_reset_count,
            reset_credits,
        }
    }
}

impl RateLimit {
    fn from_protocol(fallback_id: &str, value: &Value) -> Self {
        let id = value
            .get("limitId")
            .and_then(Value::as_str)
            .unwrap_or(fallback_id)
            .to_owned();
        let name = value
            .get("limitName")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(&id)
            .to_owned();
        let credits = value.get("credits").filter(|v| !v.is_null());
        Self {
            id,
            name,
            plan: value
                .get("planType")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            primary: value
                .get("primary")
                .filter(|v| !v.is_null())
                .and_then(RateLimitWindow::from_protocol),
            secondary: value
                .get("secondary")
                .filter(|v| !v.is_null())
                .and_then(RateLimitWindow::from_protocol),
            credits_balance: credits
                .and_then(|v| v.get("balance"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            credits_unlimited: credits
                .and_then(|v| v.get("unlimited"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }
}

impl RateLimitWindow {
    fn from_protocol(value: &Value) -> Option<Self> {
        Some(Self {
            used_percent: value.get("usedPercent")?.as_u64()?.min(100) as u32,
            resets_at: value.get("resetsAt").and_then(Value::as_i64),
            duration_minutes: value.get("windowDurationMins").and_then(Value::as_i64),
        })
    }
}

impl ResetCredit {
    fn from_protocol(value: &Value) -> Option<Self> {
        Some(Self {
            id: value.get("id")?.as_str()?.to_owned(),
            title: value
                .get("title")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("Codex usage reset")
                .to_owned(),
            description: value
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            status: value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            granted_at: value
                .get("grantedAt")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            expires_at: value.get("expiresAt").and_then(Value::as_i64),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    pub request_id: Value,
    pub method: String,
    pub title: String,
    pub detail: String,
    pub allow_session: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxChoice {
    ReadOnly,
    WorkspaceWrite,
    FullAccess,
}

impl SandboxChoice {
    pub const ALL: [Self; 3] = [Self::ReadOnly, Self::WorkspaceWrite, Self::FullAccess];
    pub fn wire(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::FullAccess => "danger-full-access",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read only",
            Self::WorkspaceWrite => "Workspace",
            Self::FullAccess => "Full access",
        }
    }
    pub fn from_wire(value: &str) -> Self {
        match value {
            "read-only" => Self::ReadOnly,
            "danger-full-access" => Self::FullAccess,
            _ => Self::WorkspaceWrite,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalChoice {
    OnRequest,
    Untrusted,
    Never,
}

impl ApprovalChoice {
    pub const ALL: [Self; 3] = [Self::OnRequest, Self::Untrusted, Self::Never];
    pub fn wire(self) -> &'static str {
        match self {
            Self::OnRequest => "on-request",
            Self::Untrusted => "untrusted",
            Self::Never => "never",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::OnRequest => "Ask when needed",
            Self::Untrusted => "Ask for risky",
            Self::Never => "Never ask",
        }
    }
    pub fn from_wire(value: &str) -> Self {
        match value {
            "untrusted" => Self::Untrusted,
            "never" => Self::Never,
            _ => Self::OnRequest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    pub workspace: String,
    pub model: String,
    pub effort: String,
    pub sandbox: SandboxChoice,
    pub approval: ApprovalChoice,
    pub show_inspector: bool,
    #[serde(default = "default_true")]
    pub respect_gitignore: bool,
    #[serde(default = "default_summary_model")]
    pub summary_model: String,
    #[serde(default = "default_summary_effort")]
    pub summary_effort: String,
}

fn default_summary_model() -> String {
    "gpt-5.6-luna".into()
}
fn default_true() -> bool {
    true
}
fn default_summary_effort() -> String {
    "low".into()
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            workspace: std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            model: String::new(),
            effort: "high".into(),
            sandbox: SandboxChoice::WorkspaceWrite,
            approval: ApprovalChoice::OnRequest,
            show_inspector: false,
            respect_gitignore: true,
            summary_model: default_summary_model(),
            summary_effort: default_summary_effort(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    #[serde(default)]
    pub preferences: Preferences,
    #[serde(default)]
    pub history: AppHistory,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_defaults_empty() {
        let history: AppHistory = serde_json::from_str("{}").unwrap();
        assert!(history.projects.is_empty());
        assert!(history.threads.is_empty());
    }

    #[test]
    fn older_preferences_default_to_respecting_gitignore() {
        let mut value = serde_json::to_value(Preferences::default()).unwrap();
        value.as_object_mut().unwrap().remove("respect_gitignore");

        let preferences: Preferences = serde_json::from_value(value).unwrap();
        assert!(preferences.respect_gitignore);
    }

    #[test]
    fn history_round_trips_unicode_and_context_usage() {
        let history = AppHistory {
            projects: vec![Project {
                id: "p".into(),
                name: "Δemo".into(),
                path: r"C:\code\demo".into(),
                created_at: 1,
            }],
            threads: vec![LocalThread {
                id: "t".into(),
                project_id: "p".into(),
                title: "Explain 🙂".into(),
                created_at: 2,
                updated_at: 3,
                title_generated: true,
                messages: vec![ConversationItem::new("m", ItemKind::User, "You")],
                context_usage: Some(ContextWindowUsage {
                    used_tokens: 24_000,
                    capacity_tokens: 120_000,
                }),
                agent: ThreadAgentSettings::default(),
            }],
            active_project: Some("p".into()),
            active_thread: Some("t".into()),
        };
        let decoded: AppHistory =
            serde_json::from_str(&serde_json::to_string(&history).unwrap()).unwrap();
        assert_eq!(decoded, history);
    }

    #[test]
    fn wire_preferences_match_codex_protocol() {
        assert_eq!(SandboxChoice::FullAccess.wire(), "danger-full-access");
        assert_eq!(ApprovalChoice::OnRequest.wire(), "on-request");
        assert_eq!(Preferences::default().summary_model, "gpt-5.6-luna");
        assert_eq!(Preferences::default().summary_effort, "low");
    }

    #[test]
    fn thread_agent_settings_copy_defaults_without_linking_to_them() {
        let mut preferences = Preferences {
            model: "gpt-5.6-sol".into(),
            effort: "high".into(),
            ..Preferences::default()
        };

        let agent = ThreadAgentSettings::from_preferences(&preferences);
        preferences.model = "gpt-5.6-luna".into();

        assert_eq!(agent.model, "gpt-5.6-sol");
        assert_eq!(agent.effort, "high");
        assert_eq!(agent.sandbox, Some(SandboxChoice::WorkspaceWrite));
    }

    #[test]
    fn parses_and_sorts_plan_usage() {
        let usage = PlanUsage::from_protocol(&serde_json::json!({
            "rateLimitsByLimitId": {
                "z": {"limitName":"Zeta","primary":{"usedPercent":150}},
                "a": {"limitName":"Alpha","primary":{"usedPercent":12}}
            },
            "rateLimitResetCredits":{"availableCount":3,"credits":null}
        }));
        assert_eq!(usage.limits[0].name, "Alpha");
        assert_eq!(usage.limits[1].primary.as_ref().unwrap().used_percent, 100);
        assert_eq!(usage.available_reset_count, 3);
        assert!(usage.reset_credits.is_none());
    }

    #[test]
    fn rejects_zero_context_capacity() {
        assert!(
            ContextWindowUsage::from_notification(
                &serde_json::json!({"tokenUsage":{"last":{"totalTokens":1},"modelContextWindow":0}})
            )
            .is_none()
        );
    }
}
