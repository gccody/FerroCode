#![cfg_attr(windows, windows_subsystem = "windows")]

use ferro_code_app::Controller;
use ferro_code_core::{LocalStore, PersistenceWorker};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

slint::include_modules!();

mod attachments;
mod callbacks;
mod desktop_services;
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

#[cfg(target_os = "macos")]
fn select_macos_backend() -> Result<(), slint::PlatformError> {
    use slint::winit_030::winit::platform::macos::WindowAttributesExtMacOS;

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_window_attributes_hook(|attributes| {
            // Keep the AppKit frame so macOS supplies its rounded corners,
            // shadow, and traffic-light controls, while letting Ferro Code's
            // header fill the title-bar area.
            attributes
                .with_decorations(true)
                .with_titlebar_transparent(true)
                .with_title_hidden(true)
                .with_fullsize_content_view(true)
                .with_has_shadow(true)
        })
        .select()
}

fn main() -> Result<(), slint::PlatformError> {
    #[cfg(windows)]
    select_windows_backend()?;
    #[cfg(target_os = "macos")]
    select_macos_backend()?;

    let store = LocalStore::discover();
    let session = match store.open_session() {
        Ok(session) => session,
        Err(error) => {
            rfd::MessageDialog::new()
                .set_title("History unavailable")
                .set_description(error.to_string())
                .set_level(rfd::MessageLevel::Error)
                .show();
            return Ok(());
        }
    };
    let (persisted, storage_warning, writable) = match session.load() {
        Ok((state, warning)) => (state, warning, true),
        Err(error) => (
            Default::default(),
            Some(format!(
                "History could not be loaded: {error}. Saving is paused. Restore a backup in Settings. Original: {}",
                store.path().display()
            )),
            false,
        ),
    };
    let writer = match PersistenceWorker::start(session, writable) {
        Ok(writer) => Arc::new(writer),
        Err(error) => {
            rfd::MessageDialog::new()
                .set_title("History unavailable")
                .set_description(error)
                .show();
            return Ok(());
        }
    };
    let saving_enabled = Rc::new(Cell::new(writable));
    let controller = Rc::new(RefCell::new(Controller::new(persisted)));
    controller.borrow_mut().start();
    let ui = MainWindow::new()?;
    ui.set_native_macos_window(cfg!(target_os = "macos"));
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
    if let Some(warning) = storage_warning {
        ui.set_storage_status(warning.clone().into());
        controller.borrow_mut().state.error(warning);
    }

    wire_callbacks(&ui, &controller, &search, &open_methods);
    let _services_timer = desktop_services::wire_services(
        &ui,
        &controller,
        &search,
        store.clone(),
        writer.clone(),
        saving_enabled.clone(),
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
    let save_writer = writer.clone();
    let save_enabled = saving_enabled.clone();
    let save_ui = ui.as_weak();
    let last_saved_revision = Rc::new(RefCell::new(0_u64));
    let save_revision = last_saved_revision.clone();
    let save_timer = Timer::default();
    save_timer.start(TimerMode::Repeated, Duration::from_secs(2), move || {
        for saved in save_writer.results.try_iter() {
            if let Err(error) = saved.result {
                if let Some(ui) = save_ui.upgrade() {
                    ui.set_storage_status(format!("History save failed: {error}").into());
                }
                *save_revision.borrow_mut() = 0;
            } else if let Some(ui) = save_ui.upgrade() {
                ui.set_storage_status("".into());
            }
        }
        if !save_enabled.get() {
            return;
        }
        let revision = save_controller.borrow().state.revision;
        if revision != *save_revision.borrow() {
            let state = save_controller.borrow_mut().persisted();
            if save_writer.save(state, revision) {
                *save_revision.borrow_mut() = revision;
            }
        }
    });

    ui.run()?;
    if saving_enabled.get()
        && let Err(error) = writer.flush(controller.borrow_mut().persisted(), false)
    {
        rfd::MessageDialog::new()
            .set_title("History could not be saved")
            .set_description(format!(
                "{error}\nThe previous saved generation is retained at {}",
                store.path().display()
            ))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
