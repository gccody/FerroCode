#![cfg_attr(windows, windows_subsystem = "windows")]

use codeagent_app::{AppState, Controller, Question};
use codeagent_core::{
    ApprovalChoice, ConversationItem, ItemKind, LocalStore, SandboxChoice, format_token_count,
    short_path,
};
use slint::{
    ComponentHandle, Image, Model, ModelRc, SharedString, StyledText, Timer, TimerMode, VecModel,
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

slint::include_modules!();

#[derive(Clone)]
struct PendingAttachment {
    path: PathBuf,
    name: String,
    preview: Image,
    is_image: bool,
}

fn main() -> Result<(), slint::PlatformError> {
    let store = LocalStore::discover();
    let persisted = store.load().unwrap_or_default();
    let controller = Rc::new(RefCell::new(Controller::new(persisted)));
    controller.borrow_mut().start();
    let ui = MainWindow::new()?;
    let search = Rc::new(RefCell::new(String::new()));
    let attachments = Rc::new(RefCell::new(Vec::<PendingAttachment>::new()));
    let attachment_temp_dir = Rc::new(tempfile::tempdir().ok());

    wire_callbacks(
        &ui,
        &controller,
        &search,
        &attachments,
        &attachment_temp_dir,
    );
    sync_ui(&ui, &controller.borrow(), &search.borrow());

    let weak_ui = ui.as_weak();
    let poll_controller = controller.clone();
    let poll_search = search.clone();
    let poll_timer = Timer::default();
    poll_timer.start(TimerMode::Repeated, Duration::from_millis(75), move || {
        if poll_controller.borrow_mut().poll()
            && let Some(ui) = weak_ui.upgrade()
        {
            sync_ui(&ui, &poll_controller.borrow(), &poll_search.borrow());
        }
    });

    let save_controller = controller.clone();
    let save_store = store.clone();
    let last_saved_revision = Rc::new(RefCell::new(0_u64));
    let save_revision = last_saved_revision.clone();
    let save_timer = Timer::default();
    save_timer.start(TimerMode::Repeated, Duration::from_secs(2), move || {
        let revision = save_controller.borrow().state.revision;
        if revision != *save_revision.borrow() {
            let state = save_controller.borrow_mut().persisted();
            if save_store.save(&state).is_ok() {
                *save_revision.borrow_mut() = revision;
            }
        }
    });

    ui.run()?;
    let _ = store.save(&controller.borrow_mut().persisted());
    Ok(())
}

fn wire_callbacks(
    ui: &MainWindow,
    controller: &Rc<RefCell<Controller>>,
    search: &Rc<RefCell<String>>,
    attachments: &Rc<RefCell<Vec<PendingAttachment>>>,
    attachment_temp_dir: &Rc<Option<tempfile::TempDir>>,
) {
    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_add_project(move || {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Add a CodeAgent project")
            .pick_folder()
        {
            controller_ref
                .borrow_mut()
                .add_project(path.to_string_lossy().into_owned());
            if let Some(ui) = weak.upgrade() {
                sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
            }
        }
    });

    callback_with_string(
        ui,
        controller,
        search,
        MainWindow::on_select_project,
        |controller, value| controller.select_project(value),
    );
    callback_with_string(
        ui,
        controller,
        search,
        MainWindow::on_toggle_project,
        |controller, value| controller.toggle_project(value),
    );
    callback_with_string(
        ui,
        controller,
        search,
        MainWindow::on_new_thread_for_project,
        |controller, value| controller.new_thread_for_project(value),
    );
    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_update_codex(move || {
        controller_ref.borrow_mut().update_codex();
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });
    callback_with_string(
        ui,
        controller,
        search,
        MainWindow::on_open_thread,
        |controller, value| controller.open_thread(value),
    );
    callback_with_string(
        ui,
        controller,
        search,
        MainWindow::on_archive_thread,
        |controller, value| controller.archive_thread(value),
    );

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_new_thread(move || {
        controller_ref.borrow_mut().new_thread();
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    let attachment_ref = attachments.clone();
    ui.on_send_message(move |text| {
        let files = std::mem::take(&mut *attachment_ref.borrow_mut())
            .into_iter()
            .map(|attachment| attachment.path.to_string_lossy().into_owned())
            .collect();
        controller_ref
            .borrow_mut()
            .send_prompt(text.to_string(), files);
        if let Some(ui) = weak.upgrade() {
            sync_attachment_ui(&ui, &attachment_ref.borrow());
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_stop_turn(move || {
        controller_ref.borrow_mut().interrupt();
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let attachment_ref = attachments.clone();
    ui.on_attach_files(move || {
        let files = rfd::FileDialog::new()
            .set_title("Attach files")
            .pick_files()
            .unwrap_or_default();
        attachment_ref
            .borrow_mut()
            .extend(files.into_iter().map(pending_attachment));
        if let Some(ui) = weak.upgrade() {
            sync_attachment_ui(&ui, &attachment_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let attachment_ref = attachments.clone();
    ui.on_remove_attachment(move |index| {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let mut attachments = attachment_ref.borrow_mut();
        if index < attachments.len() {
            attachments.remove(index);
        }
        if let Some(ui) = weak.upgrade() {
            sync_attachment_ui(&ui, &attachments);
        }
    });

    let weak = ui.as_weak();
    let attachment_ref = attachments.clone();
    let temp_dir_ref = attachment_temp_dir.clone();
    let paste_sequence = Rc::new(Cell::new(0_u64));
    ui.on_paste_image(move || {
        let clipboard_files = clipboard_file_paths();
        if !clipboard_files.is_empty() {
            attachment_ref
                .borrow_mut()
                .extend(clipboard_files.into_iter().map(pending_attachment));
            if let Some(ui) = weak.upgrade() {
                sync_attachment_ui(&ui, &attachment_ref.borrow());
            }
            return true;
        }
        let Some(temp_dir) = temp_dir_ref.as_ref() else {
            return false;
        };
        let Some((width, height, bytes)) = arboard::Clipboard::new()
            .ok()
            .and_then(|mut clipboard| clipboard.get_image().ok())
            .map(|image| (image.width, image.height, image.bytes.into_owned()))
        else {
            return false;
        };
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return false;
        };
        let sequence = paste_sequence.get().saturating_add(1);
        paste_sequence.set(sequence);
        let path = temp_dir.path().join(format!("pasted-image-{sequence}.png"));
        if image::save_buffer_with_format(
            &path,
            &bytes,
            width,
            height,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .is_err()
        {
            return false;
        }
        attachment_ref.borrow_mut().push(pending_attachment(path));
        if let Some(ui) = weak.upgrade() {
            sync_attachment_ui(&ui, &attachment_ref.borrow());
        }
        true
    });

    let weak = ui.as_weak();
    ui.on_copy_code(move |text| {
        let result = arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(text.to_string()));
        if let Some(ui) = weak.upgrade() {
            match result {
                Ok(()) => {
                    ui.set_toast_text("Code copied to clipboard".into());
                    ui.set_toast_error(false);
                }
                Err(error) => {
                    ui.set_toast_text(format!("Could not copy code: {error}").into());
                    ui.set_toast_error(true);
                }
            }
        }

        let clear_weak = weak.clone();
        Timer::single_shot(Duration::from_secs(2), move || {
            if let Some(ui) = clear_weak.upgrade() {
                ui.set_toast_text("".into());
            }
        });
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_search_threads(move |value| {
        *search_ref.borrow_mut() = value.to_string();
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_select_model(move |display_name| {
        let mut controller = controller_ref.borrow_mut();
        if let Some((id, effort, efforts)) = controller
            .state
            .models
            .iter()
            .find(|model| display_name == model.display_name)
            .map(|model| {
                (
                    model.id.clone(),
                    model.default_effort.clone(),
                    model.efforts.clone(),
                )
            })
        {
            controller.state.select_model(&id, &effort, &efforts);
        }
        drop(controller);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_select_effort(move |value| {
        let mut controller = controller_ref.borrow_mut();
        controller.state.select_effort(&value);
        drop(controller);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_select_sandbox(move |value| {
        let mut controller = controller_ref.borrow_mut();
        if let Some(agent) = controller.state.active_agent_mut() {
            agent.sandbox = Some(
                SandboxChoice::ALL
                    .into_iter()
                    .find(|choice| value == choice.label())
                    .unwrap_or(SandboxChoice::WorkspaceWrite),
            );
            controller.state.touch();
        }
        drop(controller);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_select_default_model(move |display_name| {
        let mut controller = controller_ref.borrow_mut();
        if let Some((id, effort, efforts)) = controller
            .state
            .models
            .iter()
            .find(|model| display_name == model.display_name)
            .map(|model| {
                (
                    model.id.clone(),
                    model.default_effort.clone(),
                    model.efforts.clone(),
                )
            })
        {
            controller.state.prefs.model = id;
            if !efforts.contains(&controller.state.prefs.effort) {
                controller.state.prefs.effort = effort;
            }
            controller.state.touch();
        }
        drop(controller);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_select_default_effort(move |value| {
        let mut controller = controller_ref.borrow_mut();
        controller.state.prefs.effort = value.to_string();
        controller.state.touch();
        drop(controller);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_select_default_sandbox(move |value| {
        let mut controller = controller_ref.borrow_mut();
        controller.state.prefs.sandbox = SandboxChoice::ALL
            .into_iter()
            .find(|choice| value == choice.label())
            .unwrap_or(SandboxChoice::WorkspaceWrite);
        controller.state.touch();
        drop(controller);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_select_approval(move |value| {
        let mut controller = controller_ref.borrow_mut();
        controller.state.prefs.approval = ApprovalChoice::ALL
            .into_iter()
            .find(|choice| value == choice.label())
            .unwrap_or(ApprovalChoice::OnRequest);
        controller.state.touch();
        drop(controller);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_select_summary_model(move |display_name| {
        let mut controller = controller_ref.borrow_mut();
        if let Some((id, default_effort, efforts)) = controller
            .state
            .models
            .iter()
            .find(|model| display_name == model.display_name)
            .map(|model| {
                (
                    model.id.clone(),
                    model.default_effort.clone(),
                    model.efforts.clone(),
                )
            })
        {
            controller.state.prefs.summary_model = id;
            if !efforts.contains(&controller.state.prefs.summary_effort) {
                controller.state.prefs.summary_effort =
                    if efforts.iter().any(|effort| effort == "low") {
                        "low".into()
                    } else {
                        default_effort
                    };
            }
            controller.state.touch();
        }
        drop(controller);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_select_summary_effort(move |value| {
        let mut controller = controller_ref.borrow_mut();
        controller.state.prefs.summary_effort = value.to_string();
        controller.state.touch();
        drop(controller);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_answer_approval(move |decision| {
        controller_ref.borrow_mut().answer_approval(&decision);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let controller_ref = controller.clone();
    ui.on_answer_question(move |index, answer| {
        controller_ref
            .borrow_mut()
            .set_question_answer(index as usize, answer.to_string())
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_submit_question(move || {
        controller_ref.borrow_mut().submit_question_answers();
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });

    let controller_ref = controller.clone();
    ui.on_refresh_usage(move || controller_ref.borrow_mut().refresh_plan_usage());

    let controller_ref = controller.clone();
    ui.on_consume_reset(move || controller_ref.borrow_mut().consume_reset());

    let controller_ref = controller.clone();
    ui.on_set_inspector(move |visible| {
        let mut controller = controller_ref.borrow_mut();
        if controller.state.prefs.show_inspector != visible {
            controller.state.prefs.show_inspector = visible;
            controller.state.touch();
        }
    });

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_set_respect_gitignore(move |respect| {
        let mut controller = controller_ref.borrow_mut();
        if controller.state.prefs.respect_gitignore != respect {
            controller.state.prefs.respect_gitignore = respect;
            controller.state.touch();
            controller.restart_workspace_inspection();
        }
        drop(controller);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });
}

fn callback_with_string<F, R>(
    ui: &MainWindow,
    controller: &Rc<RefCell<Controller>>,
    search: &Rc<RefCell<String>>,
    register: R,
    action: F,
) where
    F: Fn(&mut Controller, &str) + 'static,
    R: Fn(&MainWindow, Box<dyn Fn(SharedString)>) + 'static,
{
    let weak = ui.as_weak();
    let controller = controller.clone();
    let search = search.clone();
    register(
        ui,
        Box::new(move |value| {
            action(&mut controller.borrow_mut(), &value);
            if let Some(ui) = weak.upgrade() {
                sync_ui(&ui, &controller.borrow(), &search.borrow());
            }
        }),
    );
}

fn pending_attachment(path: PathBuf) -> PendingAttachment {
    let is_image = is_image_path(&path);
    let preview = if is_image {
        Image::load_from_path(&path).unwrap_or_default()
    } else {
        Image::default()
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Attachment")
        .to_owned();
    PendingAttachment {
        path,
        name,
        preview,
        is_image,
    }
}

fn sync_attachment_ui(ui: &MainWindow, attachments: &[PendingAttachment]) {
    ui.set_attachments(model(
        attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| AttachmentRow {
                index: index.min(i32::MAX as usize) as i32,
                name: attachment.name.clone().into(),
                preview: attachment.preview.clone(),
                image: attachment.is_image,
            })
            .collect::<Vec<_>>(),
    ));
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff"
            )
        })
}

#[cfg(windows)]
fn clipboard_file_paths() -> Vec<PathBuf> {
    clipboard_win::get_clipboard(clipboard_win::formats::FileList).unwrap_or_default()
}

#[cfg(not(windows))]
fn clipboard_file_paths() -> Vec<PathBuf> {
    Vec::new()
}

fn sync_ui(ui: &MainWindow, controller: &Controller, search: &str) {
    let state = &controller.state;
    ui.set_connected(state.connected);
    ui.set_connection_text(state.connection_text.clone().into());
    ui.set_account_label(state.account.label.clone().into());
    ui.set_plan_label(state.account.plan.clone().into());
    ui.set_workspace_path(short_path(&state.prefs.workspace, 54).into());
    ui.set_respect_gitignore(state.prefs.respect_gitignore);
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

    let projects = state
        .projects
        .iter()
        .map(|project| ProjectRow {
            id: project.id.clone().into(),
            name: project.name.clone().into(),
            path: project.path.clone().into(),
            active: state.active_project.as_deref() == Some(&project.id),
            collapsed: project.collapsed,
        })
        .collect::<Vec<_>>();
    if !project_rows_match(&ui.get_projects(), &projects) {
        ui.set_projects(model(projects));
    }

    let threads = thread_rows(state, search);
    if !thread_rows_match(&ui.get_threads(), &threads) {
        ui.set_threads(model(threads));
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
            .and_then(|usage| usage.limits.first())
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

    let changed = state
        .git_diff
        .lines()
        .filter(|line| line.len() > 3)
        .map(|line| line[3..].replace('/', "\\"))
        .collect::<HashSet<_>>();
    ui.set_files(model(file_rows(&state.files, &changed)));
    ui.set_git_diff(state.git_diff.clone().into());
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

fn context_ring_path(percent: u32) -> String {
    let percent = percent.min(100);
    if percent == 0 {
        return String::new();
    }
    if percent == 100 {
        return "M 10 1 A 9 9 0 0 1 10 19 A 9 9 0 0 1 10 1".into();
    }

    let angle = -std::f64::consts::FRAC_PI_2 + (f64::from(percent) / 100.0) * std::f64::consts::TAU;
    let end_x = 10.0 + 9.0 * angle.cos();
    let end_y = 10.0 + 9.0 * angle.sin();
    let large_arc = u8::from(percent > 50);
    format!("M 10 1 A 9 9 0 {large_arc} 1 {end_x:.3} {end_y:.3}")
}

fn message_height(item: &ConversationItem, markdown_blocks: &[MarkdownBlock]) -> f32 {
    if matches!(
        item.kind,
        ItemKind::Command
            | ItemKind::FileChange
            | ItemKind::Tool
            | ItemKind::Plan
            | ItemKind::System
    ) {
        return 42.0;
    }

    if item.kind == ItemKind::User {
        const MAX_CHARS: usize = 92;
        let lines = wrapped_line_count(&item.body, MAX_CHARS);
        return (lines as f32 * 17.0 + 26.0).max(48.0);
    }

    let content_height = markdown_blocks
        .iter()
        .map(|block| block.block_height)
        .sum::<f32>()
        + markdown_blocks.len().saturating_sub(1) as f32 * 7.0;
    (content_height + 10.0).max(38.0)
}

fn thread_rows(state: &AppState, search: &str) -> Vec<ThreadRow> {
    let search = search.trim().to_lowercase();
    let mut threads = state
        .threads
        .iter()
        .filter(|thread| search.is_empty() || thread.title.to_lowercase().contains(&search))
        .collect::<Vec<_>>();
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
    threads
        .into_iter()
        .map(|thread| ThreadRow {
            id: thread.id.clone().into(),
            project_id: thread.project_id.clone().into(),
            title: thread.title.clone().into(),
            subtitle: format!(
                "{} message{}",
                thread.messages.len(),
                if thread.messages.len() == 1 { "" } else { "s" }
            )
            .into(),
            active: state.active_local_thread.as_deref() == Some(&thread.id),
            busy: state.running_turns.contains_key(&thread.id),
            completed_unread: thread.unread_completion,
        })
        .collect()
}

fn project_rows_match(current: &ModelRc<ProjectRow>, next: &[ProjectRow]) -> bool {
    current.row_count() == next.len()
        && next.iter().enumerate().all(|(index, next)| {
            current.row_data(index).is_some_and(|current| {
                current.id == next.id
                    && current.name == next.name
                    && current.path == next.path
                    && current.active == next.active
                    && current.collapsed == next.collapsed
            })
        })
}

fn thread_rows_match(current: &ModelRc<ThreadRow>, next: &[ThreadRow]) -> bool {
    current.row_count() == next.len()
        && next.iter().enumerate().all(|(index, next)| {
            current.row_data(index).is_some_and(|current| {
                current.id == next.id
                    && current.project_id == next.project_id
                    && current.title == next.title
                    && current.subtitle == next.subtitle
                    && current.active == next.active
                    && current.busy == next.busy
                    && current.completed_unread == next.completed_unread
            })
        })
}

fn sync_message_rows(ui: &MainWindow, next: Vec<MessageRow>) -> bool {
    let current = ui.get_messages();
    let Some(current) = current.as_any().downcast_ref::<VecModel<MessageRow>>() else {
        ui.set_messages(model(next));
        return true;
    };
    update_message_rows(current, next)
}

fn update_message_rows(current: &VecModel<MessageRow>, next: Vec<MessageRow>) -> bool {
    let shared_len = current.row_count().min(next.len());
    let same_rows = (0..shared_len).all(|index| {
        current
            .row_data(index)
            .is_some_and(|row| row.id == next[index].id)
    });
    if !same_rows {
        current.set_vec(next);
        return true;
    }

    let mut changed = current.row_count() != next.len();
    for (index, row) in next.iter().take(shared_len).enumerate() {
        if current
            .row_data(index)
            .is_some_and(|current| !message_rows_match(&current, row))
        {
            current.set_row_data(index, row.clone());
            changed = true;
        }
    }

    while current.row_count() > next.len() {
        current.remove(current.row_count() - 1);
    }
    current.extend(next.into_iter().skip(shared_len));
    changed
}

fn message_rows_match(current: &MessageRow, next: &MessageRow) -> bool {
    current.id == next.id
        && current.kind == next.kind
        && current.title == next.title
        && current.body == next.body
        && current.status == next.status
        && current.user == next.user
        && current.activity == next.activity
        && current.row_height == next.row_height
}

fn message_rows(items: &[ConversationItem]) -> Vec<MessageRow> {
    items
        .iter()
        .filter(|item| {
            !(item.kind == ItemKind::Reasoning
                && item.body.trim().is_empty()
                && item.status != "running")
        })
        .map(|item| {
            let markdown_blocks = markdown_blocks(item);
            let row_height = message_height(item, &markdown_blocks);
            MessageRow {
                id: item.id.clone().into(),
                kind: item.kind.wire_name().into(),
                title: activity_title(item).into(),
                body: item.body.clone().into(),
                markdown_blocks: model(markdown_blocks),
                status: item.status.clone().into(),
                user: item.kind == ItemKind::User,
                activity: matches!(
                    item.kind,
                    ItemKind::Command
                        | ItemKind::FileChange
                        | ItemKind::Tool
                        | ItemKind::Plan
                        | ItemKind::System
                ),
                row_height,
            }
        })
        .collect()
}

fn wrapped_line_count(text: &str, max_chars: usize) -> usize {
    text.lines()
        .map(|line| line.chars().count().max(1).div_ceil(max_chars))
        .sum::<usize>()
        .max(1)
}

fn markdown_blocks(item: &ConversationItem) -> Vec<MarkdownBlock> {
    if !matches!(item.kind, ItemKind::Assistant | ItemKind::Reasoning) {
        return Vec::new();
    }

    parse_markdown_blocks(&item.body, item.kind == ItemKind::Reasoning)
}

fn parse_markdown_blocks(markdown: &str, reasoning: bool) -> Vec<MarkdownBlock> {
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut blocks = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        if lines[index].trim().is_empty() {
            index += 1;
            continue;
        }

        if let Some((marker, count, language)) = opening_fence(lines[index]) {
            index += 1;
            let mut code = Vec::new();
            while index < lines.len() && !closing_fence(lines[index], marker, count) {
                code.push(lines[index]);
                index += 1;
            }
            if index < lines.len() {
                index += 1;
            }
            blocks.push(code_block(&code.join("\n"), language));
            continue;
        }

        if is_indented_code(lines[index]) {
            let mut code = Vec::new();
            while index < lines.len()
                && (is_indented_code(lines[index]) || lines[index].trim().is_empty())
            {
                code.push(
                    lines[index]
                        .strip_prefix("    ")
                        .or_else(|| lines[index].strip_prefix('\t'))
                        .unwrap_or_default(),
                );
                index += 1;
            }
            blocks.push(code_block(&code.join("\n"), "text"));
            continue;
        }

        if index + 1 < lines.len()
            && let Some(alignments) = table_alignments(lines[index + 1])
        {
            let headers = split_table_row(lines[index]);
            if headers.len() == alignments.len() && headers.len() > 1 {
                index += 2;
                let mut source_rows = vec![(headers, true)];
                while index < lines.len() && !lines[index].trim().is_empty() {
                    let cells = split_table_row(lines[index]);
                    if cells.len() != alignments.len() {
                        break;
                    }
                    source_rows.push((cells, false));
                    index += 1;
                }
                let column_widths = table_column_widths(&source_rows, alignments.len());
                let rows = source_rows
                    .iter()
                    .map(|(cells, header)| table_row(cells, &alignments, &column_widths, *header))
                    .collect();
                blocks.push(table_block(rows, &column_widths));
                continue;
            }
        }

        if let Some((level, heading)) = atx_heading(lines[index]) {
            blocks.push(heading_block(level, heading));
            index += 1;
            continue;
        }

        if markdown_quote(lines[index]).is_some() {
            let mut quote = Vec::new();
            while index < lines.len() {
                let Some(content) = markdown_quote(lines[index]) else {
                    break;
                };
                quote.push(content);
                index += 1;
            }
            blocks.push(styled_block("quote", &quote.join("\n"), reasoning));
            continue;
        }

        if index + 1 < lines.len()
            && let Some(level) = setext_heading_level(lines[index + 1])
        {
            blocks.push(heading_block(level, lines[index].trim()));
            index += 2;
            continue;
        }

        if is_markdown_rule(lines[index].trim()) {
            blocks.push(MarkdownBlock {
                kind: "rule".into(),
                block_height: 9.0,
                ..empty_markdown_block()
            });
            index += 1;
            continue;
        }

        let mut paragraph = Vec::new();
        while index < lines.len() && !lines[index].trim().is_empty() {
            if !paragraph.is_empty() && starts_block(&lines, index) {
                break;
            }
            paragraph.push(lines[index]);
            index += 1;
        }
        blocks.push(styled_block("paragraph", &paragraph.join("\n"), reasoning));
    }

    if blocks.is_empty() {
        blocks.push(styled_block("paragraph", "", reasoning));
    }
    blocks
}

fn starts_block(lines: &[&str], index: usize) -> bool {
    opening_fence(lines[index]).is_some()
        || is_indented_code(lines[index])
        || atx_heading(lines[index]).is_some()
        || markdown_quote(lines[index]).is_some()
        || is_markdown_rule(lines[index].trim())
        || (index + 1 < lines.len()
            && (setext_heading_level(lines[index + 1]).is_some()
                || table_alignments(lines[index + 1]).is_some()))
}

fn empty_markdown_block() -> MarkdownBlock {
    MarkdownBlock {
        kind: "".into(),
        text: StyledText::default(),
        raw_text: "".into(),
        language: "".into(),
        level: 0,
        block_height: 0.0,
        column_count: 0,
        table_width: 0.0,
        table_height: 0.0,
        table_rows: model(Vec::<MarkdownTableRow>::new()),
    }
}

fn styled_block(kind: &str, markdown: &str, reasoning: bool) -> MarkdownBlock {
    let normalized = normalize_inline_markdown(markdown);
    let text = StyledText::from_markdown(&normalized)
        .unwrap_or_else(|_| StyledText::from_plain_text(markdown));
    let line_height = if reasoning { 15.0 } else { 16.0 };
    let height = wrapped_line_count(markdown, 112) as f32 * line_height;
    // StyledText can paint Segoe UI descenders slightly below the estimated
    // line box. Leave a little vertical room so letters such as g, p, and y
    // are not clipped at the bottom of an assistant message.
    const GLYPH_OVERFLOW: f32 = 2.0;
    MarkdownBlock {
        kind: kind.into(),
        text,
        block_height: if kind == "quote" {
            height + 14.0 + GLYPH_OVERFLOW
        } else {
            height.max(line_height) + GLYPH_OVERFLOW
        },
        ..empty_markdown_block()
    }
}

fn heading_block(level: i32, markdown: &str) -> MarkdownBlock {
    let normalized = normalize_inline_markdown(markdown);
    let text = StyledText::from_markdown(&normalized)
        .unwrap_or_else(|_| StyledText::from_plain_text(markdown));
    let height = match level {
        1 => 34.0,
        2 => 30.0,
        3 => 27.0,
        4 => 24.0,
        _ => 22.0,
    };
    MarkdownBlock {
        kind: "heading".into(),
        text,
        level,
        block_height: height,
        ..empty_markdown_block()
    }
}

fn code_block(code: &str, language: &str) -> MarkdownBlock {
    MarkdownBlock {
        kind: "code".into(),
        raw_text: code.into(),
        language: if language.is_empty() {
            "text"
        } else {
            language
        }
        .into(),
        block_height: wrapped_line_count(code, 105) as f32 * 16.0 + 42.0,
        ..empty_markdown_block()
    }
}

fn table_block(rows: Vec<MarkdownTableRow>, column_widths: &[f32]) -> MarkdownBlock {
    let height = rows.iter().map(|row| row.row_height).sum::<f32>();
    MarkdownBlock {
        kind: "table".into(),
        block_height: height + 12.0,
        column_count: column_widths.len().min(i32::MAX as usize) as i32,
        table_width: column_widths.iter().sum(),
        table_height: height,
        table_rows: model(rows),
        ..empty_markdown_block()
    }
}

fn table_column_widths(rows: &[(Vec<String>, bool)], column_count: usize) -> Vec<f32> {
    let mut widths = vec![48.0_f32; column_count];
    for (cells, _) in rows {
        for (index, cell) in cells.iter().enumerate() {
            let longest_line = cell
                .lines()
                .map(|line| markdown_visible_len(line.trim()))
                .max()
                .unwrap_or_default();
            widths[index] = widths[index].max((longest_line as f32 * 6.8 + 18.0).min(420.0));
        }
    }
    widths
}

fn table_row(
    cells: &[String],
    alignments: &[&str],
    column_widths: &[f32],
    header: bool,
) -> MarkdownTableRow {
    let lines = cells
        .iter()
        .zip(column_widths)
        .map(|(cell, width)| {
            let chars_per_line = (((*width - 18.0) / 6.8).round() as usize).max(1);
            cell.lines()
                .map(|line| {
                    markdown_visible_len(line.trim())
                        .max(1)
                        .div_ceil(chars_per_line)
                })
                .sum::<usize>()
                .max(1)
        })
        .max()
        .unwrap_or(1);
    MarkdownTableRow {
        cells: model(cells.iter().zip(alignments).zip(column_widths).map(
            |((cell, alignment), width)| {
                let normalized = normalize_inline_markdown(cell.trim());
                MarkdownTableCell {
                    text: StyledText::from_markdown(&normalized)
                        .unwrap_or_else(|_| StyledText::from_plain_text(cell.trim())),
                    alignment: (*alignment).into(),
                    column_width: *width,
                }
            },
        )),
        header,
        row_height: (lines as f32 * 16.0 + 12.0).max(30.0),
    }
}

fn markdown_visible_len(markdown: &str) -> usize {
    let mut count = 0;
    let mut link_destination = false;
    let mut chars = markdown.chars().peekable();
    while let Some(ch) = chars.next() {
        if link_destination {
            if ch == ')' {
                link_destination = false;
            }
            continue;
        }
        if ch == ']' && chars.peek() == Some(&'(') {
            chars.next();
            link_destination = true;
            continue;
        }
        if matches!(ch, '*' | '_' | '~' | '`' | '[' | ']') {
            continue;
        }
        if ch == '\\' {
            if chars.next().is_some() {
                count += 1;
            }
            continue;
        }
        count += 1;
    }
    count
}

fn opening_fence(line: &str) -> Option<(char, usize, &str)> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let count = trimmed.chars().take_while(|ch| *ch == marker).count();
    if count < 3 {
        return None;
    }
    let info = trimmed[count..].trim();
    if marker == '`' && info.contains('`') {
        return None;
    }
    let language = info.split_whitespace().next().unwrap_or_default();
    Some((marker, count, language))
}

fn closing_fence(line: &str, marker: char, count: usize) -> bool {
    let trimmed = line.trim();
    let marker_count = trimmed.chars().take_while(|ch| *ch == marker).count();
    marker_count >= count && trimmed.chars().skip(marker_count).all(char::is_whitespace)
}

fn is_indented_code(line: &str) -> bool {
    line.starts_with("    ") || line.starts_with('\t')
}

fn atx_heading(line: &str) -> Option<(i32, &str)> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let count = trimmed.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&count) {
        return None;
    }
    let heading = trimmed.get(count..)?.strip_prefix([' ', '\t'])?;
    Some((count as i32, heading.trim_end_matches('#').trim_end()))
}

fn setext_heading_level(line: &str) -> Option<i32> {
    let compact = line.trim();
    if compact.is_empty() {
        return None;
    }
    if compact.chars().all(|ch| ch == '=') {
        Some(1)
    } else if compact.chars().all(|ch| ch == '-') {
        Some(2)
    } else {
        None
    }
}

fn markdown_quote(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    trimmed
        .strip_prefix('>')
        .map(|content| content.strip_prefix(' ').unwrap_or(content))
}

fn is_markdown_rule(line: &str) -> bool {
    let compact = line
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    compact.len() >= 3
        && compact.chars().next().is_some_and(|marker| {
            matches!(marker, '-' | '*' | '_') && compact.chars().all(|ch| ch == marker)
        })
}

fn table_alignments(line: &str) -> Option<Vec<&'static str>> {
    let cells = split_table_row(line);
    if cells.len() < 2 {
        return None;
    }
    cells
        .iter()
        .map(|cell| {
            let cell = cell.trim();
            let core = cell.trim_matches(':');
            if core.len() < 3 || !core.chars().all(|ch| ch == '-') {
                return None;
            }
            Some(if cell.starts_with(':') && cell.ends_with(':') {
                "center"
            } else if cell.ends_with(':') {
                "right"
            } else {
                "left"
            })
        })
        .collect()
}

fn split_table_row(line: &str) -> Vec<String> {
    let line = line.trim().trim_start_matches('|').trim_end_matches('|');
    let mut cells = vec![String::new()];
    let mut escaped = false;
    let mut code_delimiter = 0;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if escaped {
            cells.last_mut().unwrap().extend(['\\', ch]);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '`' {
            let mut run = 1;
            while chars.next_if_eq(&'`').is_some() {
                run += 1;
            }
            if code_delimiter == 0 {
                code_delimiter = run;
            } else if code_delimiter == run {
                code_delimiter = 0;
            }
            cells.last_mut().unwrap().push_str(&"`".repeat(run));
            continue;
        }
        if ch == '|' && code_delimiter == 0 {
            cells.push(String::new());
        } else {
            cells.last_mut().unwrap().push(ch);
        }
    }
    if escaped {
        cells.last_mut().unwrap().push('\\');
    }
    cells
}

fn normalize_inline_markdown(markdown: &str) -> String {
    markdown
        .replace("- [x] ", "- ☑ ")
        .replace("- [X] ", "- ☑ ")
        .replace("- [ ] ", "- ☐ ")
        .replace("![", "[🖼 ")
        .replace('<', "\\<")
}

fn question_row(index: usize, question: &Question) -> QuestionRow {
    QuestionRow {
        index: index.min(i32::MAX as usize) as i32,
        header: question.header.clone().into(),
        question: question.question.clone().into(),
        answer: question.answer.clone().into(),
        secret: question.secret,
        option_count: question.options.len().min(3) as i32,
        option_a: question
            .options
            .first()
            .map(|(label, _)| label.as_str())
            .unwrap_or_default()
            .into(),
        option_b: question
            .options
            .get(1)
            .map(|(label, _)| label.as_str())
            .unwrap_or_default()
            .into(),
        option_c: question
            .options
            .get(2)
            .map(|(label, _)| label.as_str())
            .unwrap_or_default()
            .into(),
    }
}

fn activity_title(item: &ConversationItem) -> String {
    let running = item.status == "running";
    match item.kind {
        ItemKind::Command => if running {
            "Running command"
        } else {
            "Ran command"
        }
        .into(),
        ItemKind::FileChange => if running {
            "Changing files"
        } else {
            "Changed files"
        }
        .into(),
        ItemKind::Plan => if running {
            "Updating plan"
        } else {
            "Updated plan"
        }
        .into(),
        ItemKind::Tool if item.title.to_lowercase().contains("web search") => if running {
            "Searching the web"
        } else {
            "Searched the web"
        }
        .into(),
        ItemKind::Tool => format!("{} {}", if running { "Using" } else { "Used" }, item.title),
        _ => item.title.clone(),
    }
}

fn model<T: Clone + 'static>(values: impl IntoIterator<Item = T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(values.into_iter().collect::<Vec<_>>()))
}

#[derive(Default)]
struct FileTreeNode {
    directories: BTreeMap<String, FileTreeNode>,
    files: BTreeMap<String, String>,
}

fn file_rows(paths: &[String], changed: &HashSet<String>) -> Vec<FileRow> {
    let mut root = FileTreeNode::default();
    for path in paths {
        let components = path
            .split(['\\', '/'])
            .filter(|component| !component.is_empty())
            .collect::<Vec<_>>();
        let Some((file_name, directories)) = components.split_last() else {
            continue;
        };
        let mut node = &mut root;
        for directory in directories {
            node = node.directories.entry((*directory).to_owned()).or_default();
        }
        node.files.insert((*file_name).to_owned(), path.clone());
    }

    let mut rows = Vec::new();
    append_file_rows(&root, 0, changed, &mut rows);
    rows
}

fn append_file_rows(
    node: &FileTreeNode,
    depth: i32,
    changed: &HashSet<String>,
    rows: &mut Vec<FileRow>,
) {
    append_file_rows_with_guides(node, depth, changed, [false; 6], rows);
}

fn append_file_rows_with_guides(
    node: &FileTreeNode,
    depth: i32,
    changed: &HashSet<String>,
    guides: [bool; 6],
    rows: &mut Vec<FileRow>,
) {
    let child_count = node.directories.len() + node.files.len();
    let mut child_index = 0;

    for (name, child) in &node.directories {
        child_index += 1;
        let last = child_index == child_count;
        rows.push(FileRow {
            name: name.clone().into(),
            depth,
            directory: true,
            changed: false,
            connector_height: if last { 12 } else { 26 },
            guide0: guides[0],
            guide1: guides[1],
            guide2: guides[2],
            guide3: guides[3],
            guide4: guides[4],
            guide5: guides[5],
        });

        let mut child_guides = guides;
        if let Some(guide) = child_guides.get_mut(depth as usize) {
            *guide = !last;
        }
        append_file_rows_with_guides(child, depth + 1, changed, child_guides, rows);
    }
    for (name, path) in &node.files {
        child_index += 1;
        let last = child_index == child_count;
        rows.push(FileRow {
            name: name.clone().into(),
            depth,
            directory: false,
            changed: changed.contains(path),
            connector_height: if last { 12 } else { 26 },
            guide0: guides[0],
            guide1: guides[1],
            guide2: guides[2],
            guide3: guides[3],
            guide4: guides[4],
            guide5: guides[5],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_ring_path_tracks_empty_partial_and_full_usage() {
        assert!(context_ring_path(0).is_empty());
        assert_eq!(context_ring_path(25), "M 10 1 A 9 9 0 0 1 19.000 10.000");
        assert!(context_ring_path(75).contains("A 9 9 0 1 1"));
        assert_eq!(
            context_ring_path(100),
            "M 10 1 A 9 9 0 0 1 10 19 A 9 9 0 0 1 10 1"
        );
        assert_eq!(context_ring_path(101), context_ring_path(100));
    }

    #[test]
    fn user_message_height_grows_with_wrapped_content() {
        let mut short = ConversationItem::new("short", ItemKind::User, "User");
        short.body = "Hi".into();
        let mut long = ConversationItem::new("long", ItemKind::User, "User");
        long.body = "This is a long message ".repeat(100);

        let short_height = message_height(&short, &[]);
        let long_height = message_height(&long, &[]);

        assert_eq!(short_height, 48.0);
        assert!(long_height > short_height);
    }

    #[test]
    fn explicit_lines_and_wrapping_increase_message_row_height() {
        let mut multiline = ConversationItem::new("multiline", ItemKind::User, "User");
        multiline.body = "first\nsecond\nthird".into();
        let mut wrapped = ConversationItem::new("wrapped", ItemKind::Assistant, "Codex");
        wrapped.body = "x".repeat(113);
        let mut reasoning = ConversationItem::new("reasoning", ItemKind::Reasoning, "Reasoning");
        reasoning.body = "x".repeat(113);

        assert_eq!(message_height(&multiline, &[]), 77.0);
        assert_eq!(message_height(&wrapped, &markdown_blocks(&wrapped)), 44.0);
        assert_eq!(
            message_height(&reasoning, &markdown_blocks(&reasoning)),
            42.0
        );
        assert_eq!(wrapped_line_count("", 92), 1);
        assert_eq!(wrapped_line_count(&"x".repeat(92), 92), 1);
        assert_eq!(wrapped_line_count(&"x".repeat(93), 92), 2);
    }

    #[test]
    fn activity_items_use_compact_rows_and_past_tense_when_finished() {
        let mut command = ConversationItem::new("command", ItemKind::Command, "cargo test");
        command.status = "completed".into();

        assert_eq!(message_height(&command, &[]), 42.0);
        assert_eq!(activity_title(&command), "Ran command");
    }

    #[test]
    fn file_rows_form_a_directory_first_tree_and_preserve_changed_files() {
        let paths = vec![
            "Cargo.toml".into(),
            "src\\main.rs".into(),
            "src\\ui\\panel.rs".into(),
            "src\\lib.rs".into(),
            "tests\\app.rs".into(),
        ];
        let changed = HashSet::from(["src\\main.rs".into()]);

        let rows = file_rows(&paths, &changed);
        let summary = rows
            .iter()
            .map(|row| (row.name.to_string(), row.depth, row.directory, row.changed))
            .collect::<Vec<_>>();

        assert_eq!(
            summary,
            vec![
                ("src".into(), 0, true, false),
                ("ui".into(), 1, true, false),
                ("panel.rs".into(), 2, false, false),
                ("lib.rs".into(), 1, false, false),
                ("main.rs".into(), 1, false, true),
                ("tests".into(), 0, true, false),
                ("app.rs".into(), 1, false, false),
                ("Cargo.toml".into(), 0, false, false),
            ]
        );

        let branches = rows
            .iter()
            .map(|row| {
                (
                    row.name.to_string(),
                    row.connector_height,
                    [
                        row.guide0, row.guide1, row.guide2, row.guide3, row.guide4, row.guide5,
                    ],
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            branches,
            vec![
                ("src".into(), 26, [false; 6]),
                ("ui".into(), 26, [true, false, false, false, false, false]),
                (
                    "panel.rs".into(),
                    12,
                    [true, true, false, false, false, false]
                ),
                (
                    "lib.rs".into(),
                    26,
                    [true, false, false, false, false, false]
                ),
                (
                    "main.rs".into(),
                    12,
                    [true, false, false, false, false, false]
                ),
                ("tests".into(), 26, [false; 6]),
                (
                    "app.rs".into(),
                    12,
                    [true, false, false, false, false, false]
                ),
                ("Cargo.toml".into(), 12, [false; 6]),
            ]
        );
    }

    #[test]
    fn activity_titles_cover_running_completed_and_web_search_states() {
        let running_command = ConversationItem::new("command", ItemKind::Command, "cargo test");
        let mut file = ConversationItem::new("file", ItemKind::FileChange, "main.rs");
        file.status = "completed".into();
        let mut plan = ConversationItem::new("plan", ItemKind::Plan, "Refactor");
        plan.status = "completed".into();
        let mut search = ConversationItem::new("search", ItemKind::Tool, "Web Search");
        search.status = "completed".into();
        let tool = ConversationItem::new("tool", ItemKind::Tool, "Inspector");

        assert_eq!(activity_title(&running_command), "Running command");
        assert_eq!(activity_title(&file), "Changed files");
        assert_eq!(activity_title(&plan), "Updated plan");
        assert_eq!(activity_title(&search), "Searched the web");
        assert_eq!(activity_title(&tool), "Using Inspector");
    }

    #[test]
    fn thread_rows_include_all_projects_and_map_sidebar_state() {
        let mut state = AppState::from_persisted(codeagent_core::PersistedState::default());
        let first_project = state.add_project(r"C:\Code\First".into(), 1);
        let first_thread = state.new_thread(10).unwrap();
        let first = state
            .threads
            .iter_mut()
            .find(|thread| thread.id == first_thread)
            .unwrap();
        first.title = "Older result".into();
        first
            .messages
            .push(ConversationItem::new("m1", ItemKind::Assistant, "Codex"));

        let second_project = state.add_project(r"C:\Code\Second".into(), 2);
        let second_thread = state.new_thread(20).unwrap();
        let second = state
            .threads
            .iter_mut()
            .find(|thread| thread.id == second_thread)
            .unwrap();
        second.title = "Newest Match".into();
        second.messages.extend([
            ConversationItem::new("m2", ItemKind::User, "User"),
            ConversationItem::new("m3", ItemKind::Assistant, "Codex"),
        ]);
        state.running_turns.insert(second_thread.clone(), None);
        state
            .threads
            .iter_mut()
            .find(|thread| thread.id == first_thread)
            .unwrap()
            .unread_completion = true;

        let rows = thread_rows(&state, "");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id.as_str(), second_thread);
        assert_eq!(rows[0].project_id.as_str(), second_project);
        assert_eq!(rows[0].subtitle.as_str(), "2 messages");
        assert!(rows[0].active);
        assert!(rows[0].busy);
        assert_eq!(rows[1].project_id.as_str(), first_project);
        assert_eq!(rows[1].subtitle.as_str(), "1 message");
        assert!(rows[1].completed_unread);

        let rendered_rows = model(rows.clone());
        assert!(thread_rows_match(&rendered_rows, &rows));
        let mut changed_rows = rows.clone();
        changed_rows[0].busy = false;
        assert!(!thread_rows_match(&rendered_rows, &changed_rows));

        let filtered = thread_rows(&state, "  nEwEsT  ");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id.as_str(), second_thread);
    }

    #[test]
    fn message_rows_filter_finished_empty_reasoning_and_map_visual_flags() {
        let mut hidden = ConversationItem::new("hidden", ItemKind::Reasoning, "Reasoning");
        hidden.status = "completed".into();
        let running = ConversationItem::new("running", ItemKind::Reasoning, "Reasoning");
        let mut user = ConversationItem::new("user", ItemKind::User, "User");
        user.body = "Compact".into();
        let command = ConversationItem::new("command", ItemKind::Command, "cargo test");

        let rows = message_rows(&[hidden, running, user, command]);

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id.as_str(), "running");
        assert!(!rows[0].activity);
        assert_eq!(rows[1].id.as_str(), "user");
        assert!(rows[1].user);
        assert!(!rows[1].activity);
        assert_eq!(rows[2].id.as_str(), "command");
        assert!(rows[2].activity);
        assert_eq!(rows[2].row_height, 42.0);
    }

    #[test]
    fn streamed_message_updates_preserve_unchanged_rows_and_append_in_place() {
        let mut first = ConversationItem::new("first", ItemKind::Assistant, "Codex");
        first.body = "Stable history".into();
        first.status = "completed".into();
        let mut streaming = ConversationItem::new("streaming", ItemKind::Assistant, "Codex");
        streaming.body = "Partial".into();

        let current = VecModel::from(message_rows(&[first.clone(), streaming.clone()]));
        let unchanged_blocks = current.row_data(0).unwrap().markdown_blocks;
        streaming.body.push_str(" response");
        let mut tool = ConversationItem::new("tool", ItemKind::Tool, "Search");
        tool.status = "completed".into();

        assert!(update_message_rows(
            &current,
            message_rows(&[first, streaming, tool])
        ));

        assert_eq!(current.row_count(), 3);
        assert_eq!(
            current.row_data(0).unwrap().markdown_blocks,
            unchanged_blocks
        );
        assert_eq!(
            current.row_data(1).unwrap().body.as_str(),
            "Partial response"
        );
        assert_eq!(current.row_data(2).unwrap().id.as_str(), "tool");
    }

    #[test]
    fn replacing_conversation_resets_rows_when_message_ids_change() {
        let first = ConversationItem::new("first", ItemKind::User, "User");
        let replacement = ConversationItem::new("replacement", ItemKind::User, "User");
        let current = VecModel::from(message_rows(&[first]));

        assert!(update_message_rows(&current, message_rows(&[replacement])));

        assert_eq!(current.row_count(), 1);
        assert_eq!(current.row_data(0).unwrap().id.as_str(), "replacement");
    }

    #[test]
    fn unchanged_messages_do_not_emit_a_stream_revision() {
        let mut message = ConversationItem::new("message", ItemKind::Assistant, "Codex");
        message.body = "No changes".into();
        let current = VecModel::from(message_rows(&[message.clone()]));

        assert!(!update_message_rows(&current, message_rows(&[message])));
    }

    #[test]
    fn assistant_messages_are_rendered_as_markdown_and_user_text_stays_literal() {
        let mut assistant = ConversationItem::new("assistant", ItemKind::Assistant, "Codex");
        assistant.body = "Summary\n- **first**\n  * nested\n1. ordered\n`inline code`".into();
        let mut user = ConversationItem::new("user", ItemKind::User, "User");
        user.body = "- keep user input exact".into();

        let blocks = markdown_blocks(&assistant);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind.as_str(), "paragraph");
        assert_eq!(
            blocks[0].text,
            StyledText::from_markdown(&assistant.body).unwrap()
        );
        assert!(markdown_blocks(&user).is_empty());
    }

    #[test]
    fn markdown_blocks_receive_distinct_visual_layouts() {
        let markdown = "# Result\n> note\n---\n```rust\nlet value = **literal**;\n```";
        let blocks = parse_markdown_blocks(markdown, false);

        assert_eq!(
            blocks
                .iter()
                .map(|block| block.kind.as_str())
                .collect::<Vec<_>>(),
            ["heading", "quote", "rule", "code"]
        );
        assert_eq!(blocks[0].level, 1);
        assert_eq!(blocks[3].raw_text.as_str(), "let value = **literal**;");
        assert_eq!(blocks[3].language.as_str(), "rust");
        assert_eq!(blocks[3].block_height, 58.0);
    }

    #[test]
    fn gfm_tables_render_cells_without_the_delimiter_row() {
        let markdown = "| Name | Status | Score |\n|:-----|:------:|------:|\n| Ava | **Active** | 98 |\n| Alexandria | Pending | 87 |";
        let blocks = parse_markdown_blocks(markdown, false);

        assert_eq!(blocks.len(), 1);
        let table = &blocks[0];
        assert_eq!(table.kind.as_str(), "table");
        assert_eq!(table.column_count, 3);
        assert_eq!(table.table_rows.row_count(), 3);

        let header = table.table_rows.row_data(0).unwrap();
        assert!(header.header);
        assert_eq!(header.cells.row_count(), 3);
        assert_eq!(header.cells.row_data(0).unwrap().alignment.as_str(), "left");
        assert_eq!(
            header.cells.row_data(1).unwrap().alignment.as_str(),
            "center"
        );
        assert_eq!(
            header.cells.row_data(2).unwrap().alignment.as_str(),
            "right"
        );
        let name_width = header.cells.row_data(0).unwrap().column_width;
        let status_width = header.cells.row_data(1).unwrap().column_width;
        let score_width = header.cells.row_data(2).unwrap().column_width;
        assert!(name_width > status_width);
        assert!(status_width > score_width);
        assert_eq!(
            table
                .table_rows
                .row_data(2)
                .unwrap()
                .cells
                .row_data(0)
                .unwrap()
                .column_width,
            name_width
        );
        assert_eq!(header.row_height, 30.0);
        assert_eq!(table.table_height, 90.0);
        assert_eq!(table.block_height, 102.0);
        assert!(
            (table.table_width - (name_width + status_width + score_width)).abs() < f32::EPSILON
        );
    }

    #[test]
    fn question_rows_retain_choices_and_secret_input_semantics() {
        let question = Question {
            id: "q1".into(),
            header: "Credential".into(),
            question: "Choose or enter a token".into(),
            options: vec![
                ("Use saved".into(), "Reuse the stored token".into()),
                ("Enter new".into(), "Provide another token".into()),
            ],
            answer: "Use saved".into(),
            secret: true,
        };

        let row = question_row(4, &question);
        assert_eq!(row.index, 4);
        assert_eq!(row.option_count, 2);
        assert_eq!(row.option_a.as_str(), "Use saved");
        assert_eq!(row.option_b.as_str(), "Enter new");
        assert_eq!(row.answer.as_str(), "Use saved");
        assert!(row.secret);
    }
}
