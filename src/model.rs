use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWindowUsage {
    pub used_tokens: u64,
    pub capacity_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppHistory {
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub threads: Vec<LocalThread>,
    pub active_project: Option<String>,
    pub active_thread: Option<String>,
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

#[derive(Debug, Clone)]
pub struct ModelOption {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Default)]
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
            .map(|(id, snapshot)| RateLimit::from_protocol(id, snapshot))
            .collect::<Vec<_>>();
        limits.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        if limits.is_empty()
            && let Some(snapshot) = result.get("rateLimits").filter(|value| !value.is_null())
        {
            let id = snapshot
                .get("limitId")
                .and_then(Value::as_str)
                .unwrap_or("codex");
            limits.push(RateLimit::from_protocol(id, snapshot));
        }

        let reset_summary = result
            .get("rateLimitResetCredits")
            .filter(|value| !value.is_null());
        let available_reset_count = reset_summary
            .and_then(|summary| summary.get("availableCount"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let reset_credits = reset_summary
            .and_then(|summary| summary.get("credits"))
            .filter(|credits| !credits.is_null())
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
            .filter(|name| !name.is_empty())
            .unwrap_or(&id)
            .to_owned();
        let credits = value.get("credits").filter(|value| !value.is_null());
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
                .filter(|value| !value.is_null())
                .and_then(RateLimitWindow::from_protocol),
            secondary: value
                .get("secondary")
                .filter(|value| !value.is_null())
                .and_then(RateLimitWindow::from_protocol),
            credits_balance: credits
                .and_then(|credits| credits.get("balance"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            credits_unlimited: credits
                .and_then(|credits| credits.get("unlimited"))
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
        let id = value.get("id")?.as_str()?.to_owned();
        Some(Self {
            title: value
                .get("title")
                .and_then(Value::as_str)
                .filter(|title| !title.is_empty())
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
            id,
        })
    }
}

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    pub workspace: String,
    pub model: String,
    pub effort: String,
    pub sandbox: SandboxChoice,
    pub approval: ApprovalChoice,
    pub show_inspector: bool,
    #[serde(default = "default_summary_model")]
    pub summary_model: String,
    #[serde(default = "default_summary_effort")]
    pub summary_effort: String,
}

fn default_summary_model() -> String {
    "gpt-5.6-luna".into()
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
                .to_string(),
            model: String::new(),
            effort: "high".into(),
            sandbox: SandboxChoice::WorkspaceWrite,
            approval: ApprovalChoice::OnRequest,
            show_inspector: false,
            summary_model: default_summary_model(),
            summary_effort: default_summary_effort(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_history_defaults_empty() {
        let history: AppHistory = serde_json::from_str("{}").unwrap();
        assert!(history.projects.is_empty());
        assert!(history.threads.is_empty());
        assert!(history.active_project.is_none());
    }

    #[test]
    fn app_history_round_trips_projects_messages_and_context_usage() {
        let history = AppHistory {
            projects: vec![Project {
                id: "project-1".into(),
                name: "demo".into(),
                path: r"C:\code\demo".into(),
                created_at: 1,
            }],
            threads: vec![LocalThread {
                id: "thread-1".into(),
                project_id: "project-1".into(),
                title: "Explain the project".into(),
                created_at: 2,
                updated_at: 3,
                title_generated: true,
                messages: vec![ConversationItem {
                    id: "message-1".into(),
                    kind: ItemKind::User,
                    title: "You".into(),
                    body: "Explain the project".into(),
                    status: "completed".into(),
                    collapsed: false,
                    summary: None,
                }],
                context_usage: Some(ContextWindowUsage {
                    used_tokens: 24_000,
                    capacity_tokens: 120_000,
                }),
            }],
            active_project: Some("project-1".into()),
            active_thread: Some("thread-1".into()),
        };

        let encoded = serde_json::to_string(&history).unwrap();
        let decoded: AppHistory = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.projects[0].path, r"C:\code\demo");
        assert_eq!(decoded.threads[0].messages[0].body, "Explain the project");
        assert_eq!(
            decoded.threads[0].context_usage,
            Some(ContextWindowUsage {
                used_tokens: 24_000,
                capacity_tokens: 120_000,
            })
        );
    }

    #[test]
    fn wire_preferences_match_codex_protocol() {
        assert_eq!(SandboxChoice::FullAccess.wire(), "danger-full-access");
        assert_eq!(ApprovalChoice::OnRequest.wire(), "on-request");
        let prefs = Preferences::default();
        assert_eq!(prefs.summary_model, "gpt-5.6-luna");
        assert_eq!(prefs.summary_effort, "low");
    }

    #[test]
    fn parses_plan_usage_and_reset_credit_details() {
        let usage = PlanUsage::from_protocol(&serde_json::json!({
            "rateLimits": {
                "limitId": "codex",
                "limitName": "Codex",
                "planType": "plus",
                "primary": {
                    "usedPercent": 37,
                    "resetsAt": 1_800_000_000,
                    "windowDurationMins": 300
                },
                "secondary": {
                    "usedPercent": 81,
                    "resetsAt": 1_800_500_000,
                    "windowDurationMins": 10_080
                }
            },
            "rateLimitResetCredits": {
                "availableCount": 1,
                "credits": [{
                    "id": "reset-1",
                    "title": "Courtesy reset",
                    "description": "Resets Codex limits",
                    "status": "available",
                    "grantedAt": 1_700_000_000,
                    "expiresAt": 1_900_000_000,
                    "resetType": "codexRateLimits"
                }]
            }
        }));

        assert_eq!(usage.limits.len(), 1);
        assert_eq!(usage.limits[0].primary.as_ref().unwrap().used_percent, 37);
        assert_eq!(
            usage.limits[0].secondary.as_ref().unwrap().duration_minutes,
            Some(10_080)
        );
        assert_eq!(usage.available_reset_count, 1);
        assert_eq!(usage.reset_credits.as_ref().unwrap()[0].id, "reset-1");
    }

    #[test]
    fn preserves_unknown_reset_details_as_count_only() {
        let usage = PlanUsage::from_protocol(&serde_json::json!({
            "rateLimits": {},
            "rateLimitResetCredits": {
                "availableCount": 3,
                "credits": null
            }
        }));

        assert_eq!(usage.available_reset_count, 3);
        assert!(usage.reset_credits.is_none());
    }
}
