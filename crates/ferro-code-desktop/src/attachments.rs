use crate::AttachmentRow;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::mpsc,
};

type DecodedPreview = (PathBuf, Option<(u32, u32, Vec<u8>)>);
struct PreviewCache {
    images: HashMap<PathBuf, Image>,
    order: VecDeque<PathBuf>,
    pending: HashSet<PathBuf>,
    jobs: mpsc::SyncSender<PathBuf>,
    completed: mpsc::Receiver<DecodedPreview>,
}
impl PreviewCache {
    fn new() -> Self {
        let (jobs, receiver) = mpsc::sync_channel::<PathBuf>(32);
        let (results, completed) = mpsc::channel();
        std::thread::spawn(move || {
            for path in receiver {
                let decoded = (|| {
                    let mut reader = image::ImageReader::open(&path).ok()?;
                    let mut limits = image::Limits::default();
                    limits.max_alloc = Some(64 * 1024 * 1024);
                    limits.max_image_width = Some(8192);
                    limits.max_image_height = Some(8192);
                    reader.limits(limits);
                    let thumbnail = reader.decode().ok()?.thumbnail(184, 132).to_rgba8();
                    Some((thumbnail.width(), thumbnail.height(), thumbnail.into_raw()))
                })();
                if results.send((path, decoded)).is_err() {
                    break;
                }
            }
        });
        Self {
            images: HashMap::new(),
            order: VecDeque::new(),
            pending: HashSet::new(),
            jobs,
            completed,
        }
    }
}
thread_local! { static PREVIEWS: RefCell<PreviewCache> = RefCell::new(PreviewCache::new()); }
fn preview(path: &Path) -> Image {
    PREVIEWS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(image) = cache.images.get(path) {
            return image.clone();
        }
        if !cache.pending.contains(path) && cache.jobs.try_send(path.to_owned()).is_ok() {
            cache.pending.insert(path.to_owned());
        }
        Image::default()
    })
}
pub(super) fn poll_previews() -> bool {
    PREVIEWS.with(|cache| {
        let mut cache = cache.borrow_mut();
        let results = cache.completed.try_iter().collect::<Vec<_>>();
        let changed = !results.is_empty();
        for (path, pixels) in results {
            cache.pending.remove(&path);
            let image = pixels
                .map(|(width, height, bytes)| {
                    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                        &bytes, width, height,
                    ))
                })
                .unwrap_or_default();
            while cache.images.len() >= 128 {
                if let Some(old) = cache.order.pop_front() {
                    cache.images.remove(&old);
                }
            }
            cache.order.push_back(path.clone());
            cache.images.insert(path, image);
        }
        changed
    })
}

pub(super) fn attachment_rows(paths: &[String]) -> Vec<AttachmentRow> {
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let attachment_path = PathBuf::from(path);
            let is_image = is_image_path(&attachment_path);
            let preview = if is_image {
                preview(&attachment_path)
            } else {
                Image::default()
            };
            AttachmentRow {
                index: index.min(i32::MAX as usize) as i32,
                name: attachment_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Attachment")
                    .into(),
                preview,
                image: is_image,
            }
        })
        .collect()
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
