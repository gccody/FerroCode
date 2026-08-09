use crate::{
    MainWindow, OpenMethod, PendingAttachment, clipboard_file_paths, pending_attachment,
    sync_attachment_ui, sync_ui,
};
use ferro_code_app::Controller;
use ferro_code_core::{ApprovalChoice, SandboxChoice};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, SharedString, Timer};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

#[cfg(windows)]
use std::{
    ffi::c_void,
    sync::atomic::{AtomicI32, AtomicU32, Ordering},
};

#[cfg(windows)]
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::ScreenToClient,
    UI::{
        Input::KeyboardAndMouse::{ReleaseCapture, SetCapture},
        Shell::{DefSubclassProc, SetWindowSubclass},
        WindowsAndMessaging::{
            EnableMenuItem, GetClientRect, GetCursorPos, GetSystemMenu, GetWindowRect, HTBOTTOM,
            HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTCLOSE, HTLEFT, HTMAXBUTTON,
            HTMINBUTTON, HTRIGHT, HTSYSMENU, HTTOP, HTTOPLEFT, HTTOPRIGHT, IsZoomed,
            MENU_ITEM_FLAGS, MENU_ITEM_STATE, MF_BYCOMMAND, MFS_DISABLED, MFS_ENABLED,
            PostMessageW, SC_CLOSE, SC_KEYMENU, SC_MAXIMIZE, SC_MINIMIZE, SC_MOVE, SC_RESTORE,
            SC_SIZE, SendMessageW, SetMenuDefaultItem, TPM_LEFTALIGN, TPM_RETURNCMD,
            TrackPopupMenu, WM_CANCELMODE, WM_CAPTURECHANGED, WM_LBUTTONUP, WM_NCHITTEST,
            WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCRBUTTONUP, WM_SYSCOMMAND,
        },
    },
};

#[cfg(windows)]
static WINDOW_SCALE_MILLI: AtomicU32 = AtomicU32::new(1_000);

#[cfg(windows)]
static CAPTION_PRESSED: AtomicI32 = AtomicI32::new(0);

#[cfg(windows)]
const WINDOW_CHROME_SUBCLASS_ID: usize = 0x4341_5054;

#[cfg(windows)]
fn scaled_pixels(logical: i32) -> i32 {
    let scale = WINDOW_SCALE_MILLI.load(Ordering::Relaxed) as i32;
    (logical * scale + 500) / 1_000
}

#[cfg(windows)]
fn point_from_lparam(lparam: LPARAM) -> POINT {
    let packed = lparam.0 as u32;
    POINT {
        x: (packed as u16 as i16) as i32,
        y: ((packed >> 16) as u16 as i16) as i32,
    }
}

#[cfg(windows)]
unsafe fn caption_hit_test(hwnd: HWND, mut point: POINT) -> Option<u32> {
    if !unsafe { ScreenToClient(hwnd, &mut point) }.as_bool() {
        return None;
    }

    let mut client = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
        return None;
    }

    let header_height = scaled_pixels(36);
    if point.y < 0 || point.y >= header_height || point.x < 0 || point.x >= client.right {
        return None;
    }

    let button_width = scaled_pixels(46);
    let icon_width = scaled_pixels(34);
    let distance_from_right = client.right - point.x;
    Some(if point.x < icon_width {
        HTSYSMENU
    } else if distance_from_right <= button_width {
        HTCLOSE
    } else if distance_from_right <= button_width * 2 {
        HTMAXBUTTON
    } else if distance_from_right <= button_width * 3 {
        HTMINBUTTON
    } else {
        HTCAPTION
    })
}

#[cfg(windows)]
unsafe fn resize_hit_test(hwnd: HWND, mut point: POINT) -> Option<u32> {
    if unsafe { IsZoomed(hwnd) }.as_bool() || !unsafe { ScreenToClient(hwnd, &mut point) }.as_bool()
    {
        return None;
    }

    let mut client = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
        return None;
    }

    let border = scaled_pixels(6);
    let left = point.x < border;
    let right = point.x >= client.right - border;
    let top = point.y < border;
    let bottom = point.y >= client.bottom - border;

    match (left, right, top, bottom) {
        (true, _, true, _) => Some(HTTOPLEFT),
        (_, true, true, _) => Some(HTTOPRIGHT),
        (true, _, _, true) => Some(HTBOTTOMLEFT),
        (_, true, _, true) => Some(HTBOTTOMRIGHT),
        (true, _, _, _) => Some(HTLEFT),
        (_, true, _, _) => Some(HTRIGHT),
        (_, _, true, _) => Some(HTTOP),
        (_, _, _, true) => Some(HTBOTTOM),
        _ => None,
    }
}

#[cfg(windows)]
fn caption_button_id(hit: u32) -> i32 {
    match hit {
        HTMINBUTTON => 1,
        HTMAXBUTTON => 2,
        HTCLOSE => 3,
        _ => 0,
    }
}

#[cfg(windows)]
unsafe fn dispatch_caption_command(hwnd: HWND, button_id: i32) {
    let command = match button_id {
        1 => SC_MINIMIZE,
        2 if unsafe { IsZoomed(hwnd) }.as_bool() => SC_RESTORE,
        2 => SC_MAXIMIZE,
        3 => SC_CLOSE,
        _ => return,
    };
    unsafe {
        SendMessageW(
            hwnd,
            WM_SYSCOMMAND,
            Some(WPARAM(command as usize)),
            Some(LPARAM::default()),
        )
    };
}

#[cfg(windows)]
unsafe fn show_system_menu(hwnd: HWND, point: POINT) {
    let menu = unsafe { GetSystemMenu(hwnd, false) };
    if menu.0.is_null() {
        return;
    }

    let maximized = unsafe { IsZoomed(hwnd) }.as_bool();
    let enabled = |value| if value { MFS_ENABLED } else { MFS_DISABLED };
    let flags = |state: MENU_ITEM_STATE| MENU_ITEM_FLAGS(MF_BYCOMMAND.0 | state.0);
    unsafe {
        let _ = EnableMenuItem(menu, SC_RESTORE, flags(enabled(maximized)));
        let _ = EnableMenuItem(menu, SC_MOVE, flags(enabled(!maximized)));
        let _ = EnableMenuItem(menu, SC_SIZE, flags(enabled(!maximized)));
        let _ = EnableMenuItem(menu, SC_MINIMIZE, flags(MFS_ENABLED));
        let _ = EnableMenuItem(menu, SC_MAXIMIZE, flags(enabled(!maximized)));
        let _ = EnableMenuItem(menu, SC_CLOSE, flags(MFS_ENABLED));
        let _ = SetMenuDefaultItem(menu, SC_CLOSE, 0);
    }

    let selected = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_LEFTALIGN,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        )
    };
    if selected.0 != 0 {
        let _ = unsafe {
            PostMessageW(
                Some(hwnd),
                WM_SYSCOMMAND,
                WPARAM(selected.0 as usize),
                LPARAM::default(),
            )
        };
    }
}

#[cfg(windows)]
unsafe fn system_menu_anchor(hwnd: HWND) -> POINT {
    let mut window = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut window) }.is_err() {
        return POINT::default();
    }
    POINT {
        x: window.left,
        y: window.top + scaled_pixels(36),
    }
}

#[cfg(windows)]
unsafe extern "system" fn window_chrome_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    _reference_data: usize,
) -> LRESULT {
    if message == WM_NCHITTEST {
        let inherited = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
        if inherited.0 != HTCLIENT as isize {
            return inherited;
        }
        if let Some(hit) = unsafe { resize_hit_test(hwnd, point_from_lparam(lparam)) } {
            return LRESULT(hit as isize);
        }
        if let Some(hit) = unsafe { caption_hit_test(hwnd, point_from_lparam(lparam)) } {
            return LRESULT(hit as isize);
        }
        return inherited;
    }

    if message == WM_NCRBUTTONUP && matches!(wparam.0 as u32, HTCAPTION | HTSYSMENU) {
        unsafe { show_system_menu(hwnd, point_from_lparam(lparam)) };
        return LRESULT::default();
    }

    if message == WM_NCLBUTTONDBLCLK && wparam.0 as u32 == HTSYSMENU {
        unsafe { dispatch_caption_command(hwnd, 3) };
        return LRESULT::default();
    }

    if message == WM_SYSCOMMAND && (wparam.0 as u32 & 0xfff0) == SC_KEYMENU {
        unsafe { show_system_menu(hwnd, system_menu_anchor(hwnd)) };
        return LRESULT::default();
    }

    if message == WM_NCLBUTTONDOWN {
        let button_id = caption_button_id(wparam.0 as u32);
        if button_id != 0 {
            CAPTION_PRESSED.store(button_id, Ordering::Relaxed);
            unsafe {
                SetCapture(hwnd);
            }
            return LRESULT::default();
        }
    } else if matches!(message, WM_LBUTTONUP | WM_NCLBUTTONUP) {
        let button_id = CAPTION_PRESSED.swap(0, Ordering::Relaxed);
        if button_id != 0 {
            let _ = unsafe { ReleaseCapture() };
            if caption_hover(hwnd) == button_id {
                unsafe { dispatch_caption_command(hwnd, button_id) };
            }
            return LRESULT::default();
        } else if message == WM_NCLBUTTONUP && wparam.0 as u32 == HTSYSMENU {
            unsafe { show_system_menu(hwnd, system_menu_anchor(hwnd)) };
            return LRESULT::default();
        }
    } else if matches!(message, WM_CANCELMODE | WM_CAPTURECHANGED) {
        CAPTION_PRESSED.store(0, Ordering::Relaxed);
    }

    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(windows)]
fn window_hwnd(window: &winit::window::Window) -> Option<HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = window.window_handle().ok()?.as_raw();
    match handle {
        RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut c_void)),
        _ => None,
    }
}

#[cfg(windows)]
fn caption_hover(hwnd: HWND) -> i32 {
    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return 0;
    }
    unsafe { caption_hit_test(hwnd, point) }
        .map(caption_button_id)
        .unwrap_or_default()
}

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

pub(super) fn install_window_chrome(ui: &MainWindow) -> Timer {
    let weak = ui.as_weak();
    ui.on_drag_window(move || {
        if let Some(ui) = weak.upgrade() {
            ui.window().with_winit_window(|window| {
                let _ = window.drag_window();
            });
        }
    });

    let timer = Timer::default();
    #[cfg(windows)]
    {
        let weak = ui.as_weak();
        let subclass_installed = Rc::new(Cell::new(false));
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(16),
            move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let installed = subclass_installed.clone();
                let hover = ui
                    .window()
                    .with_winit_window(|window| {
                        WINDOW_SCALE_MILLI.store(
                            (window.scale_factor() * 1_000.0).round() as u32,
                            Ordering::Relaxed,
                        );
                        let Some(hwnd) = window_hwnd(window) else {
                            return 0;
                        };
                        if !installed.get() {
                            let succeeded = unsafe {
                                SetWindowSubclass(
                                    hwnd,
                                    Some(window_chrome_subclass),
                                    WINDOW_CHROME_SUBCLASS_ID,
                                    0,
                                )
                            }
                            .as_bool();
                            installed.set(succeeded);
                        }
                        caption_hover(hwnd)
                    })
                    .unwrap_or_default();
                ui.set_caption_hover(hover);
                ui.set_caption_pressed(CAPTION_PRESSED.load(Ordering::Relaxed));
            },
        );
    }

    timer
}

pub(super) fn wire_callbacks(
    ui: &MainWindow,
    controller: &Rc<RefCell<Controller>>,
    search: &Rc<RefCell<String>>,
    attachments: &Rc<RefCell<Vec<PendingAttachment>>>,
    attachment_temp_dir: &Rc<Option<tempfile::TempDir>>,
    open_methods: &Rc<Vec<OpenMethod>>,
) {
    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let search_ref = search.clone();
    ui.on_add_project(move || {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Add a Ferro Code project")
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

    let weak = ui.as_weak();
    let controller_ref = controller.clone();
    let open_methods_ref = open_methods.clone();
    ui.on_open_project(move |label| {
        let project_path = controller_ref
            .borrow()
            .state
            .active_project
            .as_ref()
            .and_then(|id| {
                controller_ref
                    .borrow()
                    .state
                    .projects
                    .iter()
                    .find(|project| &project.id == id)
                    .map(|project| project.path.clone())
            });
        let result = project_path
            .ok_or_else(|| "Select a project before opening it".to_owned())
            .and_then(|path| {
                open_methods_ref
                    .iter()
                    .find(|method| label == method.label())
                    .ok_or_else(|| format!("{} is not available on this machine", label))
                    .and_then(|method| method.open(std::path::Path::new(&path)))
            });

        if let Some(ui) = weak.upgrade() {
            match result {
                Ok(()) => {
                    ui.set_toast_text(format!("Opened project in {label}").into());
                    ui.set_toast_error(false);
                }
                Err(error) => {
                    ui.set_toast_text(error.into());
                    ui.set_toast_error(true);
                }
            }
            let clear_weak = weak.clone();
            Timer::single_shot(Duration::from_secs(2), move || {
                if let Some(ui) = clear_weak.upgrade() {
                    ui.set_toast_text("".into());
                }
            });
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
