use crate::{MainWindow, clipboard_file_paths, sync_ui};
use ferro_code_app::Controller;
use ferro_code_core::{LocalStore, PersistedState, PersistenceWorker};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, mpsc},
    time::Duration,
};

enum Job {
    Attach(String, Vec<PathBuf>),
    Paste(String, u32, u32, Vec<u8>),
    Export(Box<PersistedState>, PathBuf),
    Restore(PathBuf),
    Diagnostics(serde_json::Value, PathBuf),
}
enum Completed {
    Attached(String, Vec<String>),
    Restored(Box<PersistedState>),
    Message(String),
    Error(String, bool),
}

pub(super) fn wire_services(
    ui: &MainWindow,
    controller: &Rc<RefCell<Controller>>,
    search: &Rc<RefCell<String>>,
    store: LocalStore,
    writer: Arc<PersistenceWorker>,
    saving_enabled: Rc<Cell<bool>>,
) -> Timer {
    let (jobs, receiver) = mpsc::sync_channel::<Job>(8);
    let (results, completed) = mpsc::channel();
    std::thread::Builder::new()
        .name("desktop-files".into())
        .spawn(move || {
            for job in receiver {
                let restoring = matches!(&job, Job::Restore(_));
                let result: Result<Completed, String> = (|| match job {
                    Job::Attach(key, files) => {
                        let paths = files
                            .into_iter()
                            .map(|path| {
                                store
                                    .import_attachment(&path)
                                    .map(|path| path.to_string_lossy().into_owned())
                                    .map_err(|e| e.to_string())
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok(Completed::Attached(key, paths))
                    }
                    Job::Paste(key, width, height, pixels) => {
                        let dir = store.attachments_dir();
                        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                        let path =
                            dir.join(format!("{}.png", ferro_code_core::new_id("pasted-image")));
                        image::save_buffer_with_format(
                            &path,
                            &pixels,
                            width,
                            height,
                            image::ColorType::Rgba8,
                            image::ImageFormat::Png,
                        )
                        .map_err(|e| e.to_string())?;
                        std::fs::File::open(&path)
                            .and_then(|file| file.sync_all())
                            .map_err(|e| e.to_string())?;
                        Ok(Completed::Attached(
                            key,
                            vec![path.to_string_lossy().into_owned()],
                        ))
                    }
                    Job::Export(state, path) => {
                        store.export(&state, &path).map_err(|e| e.to_string())?;
                        Ok(Completed::Message(format!(
                            "Backup exported to {}",
                            path.display()
                        )))
                    }
                    Job::Restore(path) => {
                        let state = store.import(&path).map_err(|e| e.to_string())?;
                        writer.flush(state.clone(), true)?;
                        Ok(Completed::Restored(Box::new(state)))
                    }
                    Job::Diagnostics(value, path) => {
                        let data = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
                        std::fs::write(&path, data).map_err(|e| e.to_string())?;
                        Ok(Completed::Message(format!(
                            "Diagnostics exported to {}",
                            path.display()
                        )))
                    }
                })();
                if results
                    .send(result.unwrap_or_else(|error| Completed::Error(error, restoring)))
                    .is_err()
                {
                    break;
                }
            }
        })
        .expect("start desktop file worker");

    let control = controller.clone();
    let jobs_ref = jobs.clone();
    ui.on_attach_files(move || {
        if let Some(files) = rfd::FileDialog::new()
            .set_title("Attach files")
            .pick_files()
        {
            let key = control.borrow().state.draft_key();
            if jobs_ref.try_send(Job::Attach(key, files)).is_err() {
                control
                    .borrow_mut()
                    .state
                    .error("File processing is busy. Try attaching again.");
            } else {
                control.borrow_mut().state.info("Preparing attachments…");
            }
        }
    });
    let control = controller.clone();
    let jobs_ref = jobs.clone();
    ui.on_paste_image(move || {
        let key = control.borrow().state.draft_key();
        let files = clipboard_file_paths();
        let job = if !files.is_empty() {
            Job::Attach(key, files)
        } else {
            let Some(image) = arboard::Clipboard::new()
                .ok()
                .and_then(|mut c| c.get_image().ok())
            else {
                return false;
            };
            let (Ok(width), Ok(height)) = (u32::try_from(image.width), u32::try_from(image.height))
            else {
                return false;
            };
            Job::Paste(key, width, height, image.bytes.into_owned())
        };
        if jobs_ref.try_send(job).is_err() {
            control
                .borrow_mut()
                .state
                .error("File processing is busy. Paste again after it finishes.");
        } else {
            control.borrow_mut().state.info("Preparing attachment…");
        }
        true
    });
    let control = controller.clone();
    ui.on_remove_attachment(move |index| {
        let mut controller = control.borrow_mut();
        let mut draft = controller.state.active_draft();
        if let Ok(index) = usize::try_from(index)
            && index < draft.attachments.len()
        {
            draft.attachments.remove(index);
            controller.state.set_draft(draft);
        }
    });
    let control = controller.clone();
    ui.on_reconnect(move || control.borrow_mut().start());
    let control = controller.clone();
    ui.on_retry_prompt(move || control.borrow_mut().restore_last_prompt());
    let control = controller.clone();
    ui.on_undo_archive(move || {
        control.borrow_mut().state.undo_archive();
        control.borrow_mut().restart_workspace_inspection();
    });
    let control = controller.clone();
    let jobs_ref = jobs.clone();
    ui.on_export_history(move || {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("ferro-code-backup.json")
            .save_file()
        {
            let state = control.borrow_mut().persisted();
            if jobs_ref
                .try_send(Job::Export(Box::new(state), path))
                .is_err()
            {
                control
                    .borrow_mut()
                    .state
                    .error("File processing is busy. Try export again.");
            }
        }
    });
    let control = controller.clone();
    let jobs_ref = jobs.clone();
    let saving = saving_enabled.clone();
    let previous_saving = Rc::new(Cell::new(saving.get()));
    let restore_previous = previous_saving.clone();
    let restoring = Rc::new(Cell::new(false));
    let restore_busy = restoring.clone();
    let restore_ui = ui.as_weak();
    ui.on_restore_history(move || {
        if restore_busy.get() {
            return;
        }
        if !control.borrow().state.running_turns.is_empty()
            || control.borrow().state.git_action_in_progress
        {
            control
                .borrow_mut()
                .state
                .error("Stop running tasks before restoring history.");
            return;
        }
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Restore Ferro Code backup")
            .add_filter("History", &["json"])
            .pick_file()
        {
            if jobs_ref.try_send(Job::Restore(path)).is_err() {
                control
                    .borrow_mut()
                    .state
                    .error("File processing is busy. Try restore again.");
            } else {
                restore_previous.set(saving.replace(false));
                restore_busy.set(true);
                if let Some(ui) = restore_ui.upgrade() {
                    ui.set_storage_busy(true);
                }
                control.borrow_mut().state.info("Restoring history…");
            }
        }
    });
    let control = controller.clone();
    ui.on_export_diagnostics(move || {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("ferro-code-diagnostics.json")
            .save_file()
        {
            let value = control.borrow().diagnostics();
            if jobs.try_send(Job::Diagnostics(value, path)).is_err() {
                control
                    .borrow_mut()
                    .state
                    .error("File processing is busy. Try diagnostics again.");
            }
        }
    });
    let weak = ui.as_weak();
    let control = controller.clone();
    let search = search.clone();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(75), move || {
        let mut changed = crate::attachments::poll_previews();
        for result in completed.try_iter() {
            changed = true;
            let mut controller = control.borrow_mut();
            match result {
                Completed::Attached(key, paths) => {
                    controller
                        .state
                        .drafts
                        .entry(key)
                        .or_default()
                        .attachments
                        .extend(paths);
                    controller.state.touch();
                }
                Completed::Restored(state) => {
                    controller.restore_history(*state);
                    saving_enabled.set(true);
                    restoring.set(false);
                    if let Some(ui) = weak.upgrade() {
                        ui.set_storage_busy(false);
                    }
                    if let Some(ui) = weak.upgrade() {
                        ui.set_storage_status("".into());
                    }
                    controller
                        .state
                        .info("History restored. Previous data was preserved.");
                }
                Completed::Message(message) => controller.state.info(message),
                Completed::Error(error, restore) => {
                    if restore {
                        saving_enabled.set(previous_saving.get());
                        restoring.set(false);
                        if let Some(ui) = weak.upgrade() {
                            ui.set_storage_busy(false);
                        }
                    }
                    controller.state.error(error);
                }
            }
        }
        if changed && let Some(ui) = weak.upgrade() {
            sync_ui(&ui, &control.borrow(), &search.borrow());
        }
    });
    timer
}
