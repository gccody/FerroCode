use crate::{
    MainWindow, ProjectRow, change_rows, context_ring_path, file_rows, message_rows, model,
    project_rows_match, question_row, sync_message_rows, thread_rows, thread_rows_match,
};
use codeagent_app::{AppState, Controller};
use codeagent_core::{ApprovalChoice, SandboxChoice, format_token_count, short_path};
use slint::SharedString;
use std::collections::{BTreeMap, HashSet};

pub(super) fn sync_ui(ui: &MainWindow, controller: &Controller, search: &str) {
    let state = &controller.state;
    if ui.get_startup_loading() && !controller.startup_in_progress() {
        ui.set_startup_loading(false);
    }
    ui.set_connected(state.connected);
    ui.set_connection_text(state.connection_text.clone().into());
    ui.set_account_label(state.account.label.clone().into());
    ui.set_plan_label(state.account.plan.clone().into());
    ui.set_workspace_path(short_path(&state.prefs.workspace, 54).into());
    ui.set_respect_gitignore(state.prefs.respect_gitignore);
    ui.set_visible_thread_limit(state.prefs.visible_thread_limit.clamp(1, 100) as i32);
    ui.set_workspace_name(
        state
            .active_project
            .as_ref()
            .and_then(|id| state.projects.iter().find(|project| &project.id == id))
            .map(|project| project.name.as_str())
            .unwrap_or("Workspace")
            .into(),
    );
    ui.set_has_project(state.active_project.is_some());
    ui.set_busy(state.active_thread_busy());
    ui.set_codex_update_version(
        state
            .codex_update_version
            .clone()
            .unwrap_or_default()
            .into(),
    );
    ui.set_codex_update_in_progress(state.codex_update_in_progress);
    ui.set_inspector_visible(state.prefs.show_inspector || ui.get_inspector_visible());

    let threads = sync_thread_rows(ui, state, search);
    let mut thread_counts = BTreeMap::<String, i32>::new();
    for thread in &threads {
        *thread_counts
            .entry(thread.project_id.to_string())
            .or_default() += 1;
    }

    let projects = state
        .projects
        .iter()
        .map(|project| ProjectRow {
            id: project.id.clone().into(),
            name: project.name.clone().into(),
            path: project.path.clone().into(),
            active: state.active_project.as_deref() == Some(&project.id),
            collapsed: project.collapsed,
            thread_count: thread_counts.get(&project.id).copied().unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    if !project_rows_match(&ui.get_projects(), &projects) {
        ui.set_projects(model(projects));
    }

    ui.set_active_thread_id(state.active_local_thread.clone().unwrap_or_default().into());
    if sync_message_rows(ui, message_rows(&state.conversation)) {
        let revision = ui.get_message_revision();
        ui.set_message_revision(if revision == i32::MAX {
            0
        } else {
            revision + 1
        });
    }

    let title = state
        .active_local_thread
        .as_ref()
        .and_then(|id| state.threads.iter().find(|thread| &thread.id == id))
        .map(|thread| thread.title.as_str())
        .unwrap_or("New thread");
    ui.set_active_thread_title(title.into());
    let usage = state
        .active_local_thread
        .as_ref()
        .and_then(|id| state.threads.iter().find(|thread| &thread.id == id))
        .and_then(|thread| thread.context_usage);
    ui.set_context_label(
        usage
            .map(|usage| {
                format!(
                    "Context: {} / {} ({}%)",
                    format_token_count(usage.used_tokens),
                    format_token_count(usage.capacity_tokens),
                    usage.percent()
                )
            })
            .unwrap_or_default()
            .into(),
    );
    let context_percent = usage.map(|usage| usage.percent()).unwrap_or(0);
    ui.set_context_percent(context_percent as i32);
    ui.set_context_progress_path(context_ring_path(context_percent).into());
    ui.set_usage_left_label(
        state
            .plan_usage
            .as_ref()
            .and_then(|usage| usage.default_limit())
            .and_then(|limit| limit.primary.as_ref())
            .map(|window| {
                format!(
                    "{}% usage left",
                    100_u32.saturating_sub(window.used_percent)
                )
            })
            .unwrap_or_default()
            .into(),
    );

    let changes = change_rows(&state.git_diff);
    let changed = changes
        .iter()
        .map(|change| change.path.to_string().replace('/', "\\"))
        .collect::<HashSet<_>>();
    ui.set_files(model(file_rows(&state.files, &changed)));
    ui.set_change_count(changes.len() as i32);
    ui.set_staged_change_count(changes.iter().filter(|change| change.staged).count() as i32);
    ui.set_unstaged_change_count(changes.iter().filter(|change| change.unstaged).count() as i32);
    ui.set_changes(model(changes.into_iter().map(|change| change.row)));
    ui.set_activity(model(
        state
            .activity_log
            .iter()
            .rev()
            .take(100)
            .cloned()
            .map(SharedString::from),
    ));

    let model_names = state
        .models
        .iter()
        .map(|model| SharedString::from(model.display_name.clone()))
        .collect::<Vec<_>>();
    let efforts_for = |model_id: &str, fallback: &str| {
        state
            .models
            .iter()
            .find(|model| model.id == model_id)
            .map(|model| {
                if model.efforts.is_empty() {
                    vec![fallback.to_owned()]
                } else {
                    model.efforts.clone()
                }
            })
            .unwrap_or_else(|| vec![fallback.to_owned()])
    };
    let active_agent = state.active_agent();
    let selected_model = state
        .models
        .iter()
        .position(|model| model.id == active_agent.model)
        .unwrap_or(0);
    let efforts = efforts_for(&active_agent.model, &active_agent.effort);
    let selected_effort = efforts
        .iter()
        .position(|effort| effort == &active_agent.effort)
        .unwrap_or(0);
    let default_model = state
        .models
        .iter()
        .position(|model| model.id == state.prefs.model)
        .unwrap_or(0);
    let default_efforts = efforts_for(&state.prefs.model, &state.prefs.effort);
    let default_effort = default_efforts
        .iter()
        .position(|effort| effort == &state.prefs.effort)
        .unwrap_or(0);
    let summary_model = state
        .models
        .iter()
        .position(|model| model.id == state.prefs.summary_model)
        .unwrap_or(0);
    let summary_efforts = efforts_for(&state.prefs.summary_model, &state.prefs.summary_effort);
    let summary_effort = summary_efforts
        .iter()
        .position(|effort| effort == &state.prefs.summary_effort)
        .unwrap_or(0);
    ui.set_model_names(model(model_names));
    ui.set_selected_model_index(selected_model as i32);
    ui.set_effort_names(model(efforts.iter().cloned().map(SharedString::from)));
    ui.set_selected_effort_index(selected_effort as i32);
    ui.set_default_model_index(default_model as i32);
    ui.set_default_effort_names(model(
        default_efforts.iter().cloned().map(SharedString::from),
    ));
    ui.set_default_effort_index(default_effort as i32);
    ui.set_summary_model_index(summary_model as i32);
    ui.set_summary_effort_names(model(
        summary_efforts.iter().cloned().map(SharedString::from),
    ));
    ui.set_summary_effort_index(summary_effort as i32);
    ui.set_sandbox_names(model(
        SandboxChoice::ALL
            .into_iter()
            .map(|choice| SharedString::from(choice.label())),
    ));
    ui.set_selected_sandbox_index(
        SandboxChoice::ALL
            .iter()
            .position(|choice| *choice == active_agent.sandbox.unwrap_or(state.prefs.sandbox))
            .unwrap_or(1) as i32,
    );
    ui.set_default_sandbox_index(
        SandboxChoice::ALL
            .iter()
            .position(|choice| *choice == state.prefs.sandbox)
            .unwrap_or(1) as i32,
    );
    ui.set_approval_names(model(
        ApprovalChoice::ALL
            .into_iter()
            .map(|choice| SharedString::from(choice.label())),
    ));
    ui.set_selected_approval_index(
        ApprovalChoice::ALL
            .iter()
            .position(|choice| *choice == state.prefs.approval)
            .unwrap_or(0) as i32,
    );
    let usage_summary = state
        .plan_usage
        .as_ref()
        .map(|usage| {
            usage
                .limits
                .iter()
                .flat_map(|limit| {
                    [limit.primary.as_ref(), limit.secondary.as_ref()]
                        .into_iter()
                        .flatten()
                        .map(|window| format!("{}: {}% used", limit.name, window.used_percent))
                })
                .collect::<Vec<_>>()
                .join(" · ")
        })
        .filter(|summary| !summary.is_empty())
        .unwrap_or_else(|| "Usage information unavailable".into());
    ui.set_usage_summary(usage_summary.into());
    ui.set_reset_count(
        state
            .plan_usage
            .as_ref()
            .map(|usage| usage.available_reset_count.min(i32::MAX as u64) as i32)
            .unwrap_or(0),
    );
    ui.set_reset_in_progress(state.reset_in_progress);

    if let Some(approval) = &state.approval {
        ui.set_approval_title(approval.title.clone().into());
        ui.set_approval_detail(approval.detail.clone().into());
        ui.set_approval_session(approval.allow_session);
    } else {
        ui.set_approval_title("".into());
        ui.set_approval_detail("".into());
        ui.set_approval_session(false);
    }

    ui.set_questions(model(
        state
            .user_question
            .as_ref()
            .into_iter()
            .flat_map(|request| request.questions.iter())
            .enumerate()
            .map(|(index, question)| question_row(index, question)),
    ));

    if let Some(toast) = &state.toast {
        ui.set_toast_text(toast.message.clone().into());
        ui.set_toast_error(toast.is_error);
    } else {
        ui.set_toast_text("".into());
    }
}

pub(super) fn sync_thread_rows(
    ui: &MainWindow,
    state: &AppState,
    search: &str,
) -> Vec<crate::ThreadRow> {
    let threads = thread_rows(state, search);
    if !thread_rows_match(&ui.get_threads(), &threads) {
        ui.set_threads(model(threads.clone()));
    }
    threads
}
