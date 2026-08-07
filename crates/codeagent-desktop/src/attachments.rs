use crate::{AttachmentRow, MainWindow, model};
use slint::Image;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(super) struct PendingAttachment {
    pub(super) path: PathBuf,
    pub(super) name: String,
    pub(super) preview: Image,
    pub(super) is_image: bool,
}

pub(super) fn pending_attachment(path: PathBuf) -> PendingAttachment {
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

pub(super) fn sync_attachment_ui(ui: &MainWindow, attachments: &[PendingAttachment]) {
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

pub(super) fn is_image_path(path: &Path) -> bool {
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
pub(super) fn clipboard_file_paths() -> Vec<PathBuf> {
    clipboard_win::get_clipboard(clipboard_win::formats::FileList).unwrap_or_default()
}

#[cfg(not(windows))]
pub(super) fn clipboard_file_paths() -> Vec<PathBuf> {
    Vec::new()
}
