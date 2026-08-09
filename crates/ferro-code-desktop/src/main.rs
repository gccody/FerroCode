#![cfg_attr(windows, windows_subsystem = "windows")]

use ferro_code_app::Controller;
use ferro_code_core::LocalStore;
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

slint::include_modules!();

mod attachments;
mod callbacks;
mod markdown;
mod project_launcher;
mod sync;
mod view_models;
mod workspace_view;

use attachments::*;
use callbacks::*;
use markdown::*;
use project_launcher::*;
use sync::*;
use view_models::*;
use workspace_view::*;

#[cfg(windows)]
fn select_windows_backend() -> Result<(), slint::PlatformError> {
    use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;

    let pixels = image::load_from_memory_with_format(
        include_bytes!("../assets/app-icon.png"),
        image::ImageFormat::Png,
    )
    .expect("decode embedded application icon")
    .into_rgba8();
    let (width, height) = pixels.dimensions();
    let taskbar_icon =
        slint::winit_030::winit::window::Icon::from_rgba(pixels.into_raw(), width, height)
            .expect("create Windows taskbar icon");

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_window_attributes_hook(move |attributes| {
            attributes.with_taskbar_icon(Some(taskbar_icon.clone()))
        })
        .select()
}

fn main() -> Result<(), slint::PlatformError> {
    #[cfg(windows)]
    select_windows_backend()?;

    let store = LocalStore::discover();
    let persisted = store.load().unwrap_or_default();
    let controller = Rc::new(RefCell::new(Controller::new(persisted)));
    controller.borrow_mut().start();
    let ui = MainWindow::new()?;
    let open_methods = Rc::new(available_open_methods());
    ui.set_open_project_methods(model(open_methods.iter().map(|method| {
        let icon = method.icon();
        OpenMethodRow {
            label: method.label().into(),
            has_icon: icon.is_some(),
            icon: icon.unwrap_or_default(),
        }
    })));
    install_input_focus_dismissal(&ui);
    let _window_chrome_timer = install_window_chrome(&ui);
    let search = Rc::new(RefCell::new(String::new()));
    let attachments = Rc::new(RefCell::new(Vec::<PendingAttachment>::new()));
    let attachment_temp_dir = Rc::new(tempfile::tempdir().ok());

    wire_callbacks(
        &ui,
        &controller,
        &search,
        &attachments,
        &attachment_temp_dir,
        &open_methods,
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

    let elapsed_controller = controller.clone();
    let elapsed_ui = ui.as_weak();
    let elapsed_timer = Timer::default();
    elapsed_timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let label = elapsed_controller
            .borrow()
            .state
            .active_turn_elapsed_ms(now_ms)
            .map(elapsed_duration_label)
            .unwrap_or_default();
        if let Some(ui) = elapsed_ui.upgrade() {
            ui.set_turn_elapsed_label(label.into());
        }
    });

    let thread_age_controller = controller.clone();
    let thread_age_ui = ui.as_weak();
    let thread_age_search = search.clone();
    let thread_age_timer = Timer::default();
    thread_age_timer.start(TimerMode::Repeated, Duration::from_secs(30), move || {
        if let Some(ui) = thread_age_ui.upgrade() {
            sync_thread_rows(
                &ui,
                &thread_age_controller.borrow().state,
                &thread_age_search.borrow(),
            );
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

#[cfg(test)]
mod tests;
