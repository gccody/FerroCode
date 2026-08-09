use crate::{
    MainWindow, PendingAttachment, clipboard_file_paths, pending_attachment, sync_attachment_ui,
    sync_ui,
};
use codeagent_app::Controller;
use codeagent_core::{ApprovalChoice, SandboxChoice};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, SharedString, Timer};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

pub(super) fn install_input_focus_dismissal(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.window().on_winit_window_event(move |_, event| {
        // This runs before Slint dispatches the press. A clicked input will
        // focus itself again; all other targets leave editable controls unfocused.
        let pointer_pressed = matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            } | winit::event::WindowEvent::Touch(winit::event::Touch {
                phase: winit::event::TouchPhase::Started,
                ..
            })
        );

        if pointer_pressed && let Some(ui) = weak.upgrade() {
            ui.invoke_clear_input_focus();
        }

        EventResult::Propagate
    });
}

pub(super) fn wire_callbacks(
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
    callback_with_string(
        ui,
        controller,
        search,
        MainWindow::on_toggle_message,
        |controller, value| controller.toggle_message(value),
    );
    callback_with_string(
        ui,
        controller,
        search,
        MainWindow::on_toggle_response_details,
        |controller, value| controller.toggle_response_details(value),
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
    ui.on_copy_response(move |text| {
        let result = arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(text.to_string()));
        if let Some(ui) = weak.upgrade() {
            match result {
                Ok(()) => {
                    ui.set_toast_text("Response copied to clipboard".into());
                    ui.set_toast_error(false);
                }
                Err(error) => {
                    ui.set_toast_text(format!("Could not copy response: {error}").into());
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
    ui.on_refresh_workspace(move || controller_ref.borrow_mut().restart_workspace_inspection());

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

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_set_visible_thread_limit(move |limit| {
        let limit = limit.clamp(1, 100) as u32;
        let mut controller = controller_ref.borrow_mut();
        if controller.state.prefs.visible_thread_limit != limit {
            controller.state.prefs.visible_thread_limit = limit;
            controller.state.touch();
        }
        drop(controller);
        if let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &controller_ref.borrow(), &search_ref.borrow());
        }
    });
}

pub(super) fn callback_with_string<F, R>(
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
