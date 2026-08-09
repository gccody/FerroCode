use crate::{
    MainWindow, MarkdownBlock, MessageRow, ProjectRow, QuestionRow, ThreadRow, markdown_blocks,
    wrapped_line_count,
};
use codeagent_app::{AppState, Question};
use codeagent_core::{ConversationItem, ItemKind};
use slint::{Model, ModelRc, VecModel};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) fn context_ring_path(percent: u32) -> String {
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

pub(super) fn message_height(item: &ConversationItem, markdown_blocks: &[MarkdownBlock]) -> f32 {
    if item.collapsed
        && matches!(
            item.kind,
            ItemKind::Command
                | ItemKind::FileChange
                | ItemKind::Tool
                | ItemKind::Plan
                | ItemKind::System
        )
    {
        return 38.0;
    }

    if matches!(
        item.kind,
        ItemKind::Command
            | ItemKind::FileChange
            | ItemKind::Tool
            | ItemKind::Plan
            | ItemKind::System
    ) {
        // Expanded activity rows are measured by Slint from the rendered Text.
        // A fixed estimate accumulates visible error over long command output.
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
    let copy_action_height = if item.kind == ItemKind::Assistant && item.duration_ms.is_some() {
        35.0
    } else {
        0.0
    };
    (content_height + copy_action_height + 10.0).max(38.0)
}

pub(super) fn elapsed_duration_label(duration_ms: u64) -> String {
    let seconds = duration_ms / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

pub(super) fn relative_time_label(timestamp: i64, now: i64) -> String {
    let elapsed = now.saturating_sub(timestamp).max(0) as u64;
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    const MONTH: u64 = 30 * DAY;
    const YEAR: u64 = 365 * DAY;

    if elapsed < MINUTE {
        "now".into()
    } else if elapsed < HOUR {
        format!("{}m ago", elapsed / MINUTE)
    } else if elapsed < DAY {
        format!("{}h ago", elapsed / HOUR)
    } else if elapsed < MONTH {
        format!("{}d ago", elapsed / DAY)
    } else if elapsed < YEAR {
        format!("{}mo ago", elapsed / MONTH)
    } else {
        format!("{}y ago", elapsed / YEAR)
    }
}

pub(super) fn thread_rows(state: &AppState, search: &str) -> Vec<ThreadRow> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64;
    thread_rows_at(state, search, now)
}

fn thread_rows_at(state: &AppState, search: &str, now: i64) -> Vec<ThreadRow> {
    let search = search.trim().to_lowercase();
    let mut threads = state
        .threads
        .iter()
        .filter(|thread| search.is_empty() || thread.title.to_lowercase().contains(&search))
        .collect::<Vec<_>>();
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
    let mut project_indexes = BTreeMap::<&str, i32>::new();
    threads
        .into_iter()
        .map(|thread| {
            let project_index = project_indexes.entry(&thread.project_id).or_default();
            let row = ThreadRow {
                id: thread.id.clone().into(),
                project_id: thread.project_id.clone().into(),
                title: thread.title.clone().into(),
                subtitle: format!(
                    "{} message{}",
                    thread.messages.len(),
                    if thread.messages.len() == 1 { "" } else { "s" }
                )
                .into(),
                age_label: if thread.messages.is_empty() {
                    String::new()
                } else {
                    relative_time_label(thread.updated_at, now)
                }
                .into(),
                active: state.active_local_thread.as_deref() == Some(&thread.id),
                busy: state.running_turns.contains_key(&thread.id),
                completed_unread: thread.unread_completion,
                project_index: *project_index,
            };
            *project_index += 1;
            row
        })
        .collect()
}

pub(super) fn project_rows_match(current: &ModelRc<ProjectRow>, next: &[ProjectRow]) -> bool {
    current.row_count() == next.len()
        && next.iter().enumerate().all(|(index, next)| {
            current.row_data(index).is_some_and(|current| {
                current.id == next.id
                    && current.name == next.name
                    && current.path == next.path
                    && current.active == next.active
                    && current.collapsed == next.collapsed
                    && current.thread_count == next.thread_count
            })
        })
}

pub(super) fn thread_rows_match(current: &ModelRc<ThreadRow>, next: &[ThreadRow]) -> bool {
    current.row_count() == next.len()
        && next.iter().enumerate().all(|(index, next)| {
            current.row_data(index).is_some_and(|current| {
                current.id == next.id
                    && current.project_id == next.project_id
                    && current.title == next.title
                    && current.subtitle == next.subtitle
                    && current.age_label == next.age_label
                    && current.active == next.active
                    && current.busy == next.busy
                    && current.completed_unread == next.completed_unread
                    && current.project_index == next.project_index
            })
        })
}

pub(super) fn sync_message_rows(ui: &MainWindow, next: Vec<MessageRow>) -> bool {
    let current = ui.get_messages();
    let Some(current) = current.as_any().downcast_ref::<VecModel<MessageRow>>() else {
        ui.set_messages(model(next));
        return true;
    };
    update_message_rows(current, next)
}

pub(super) fn update_message_rows(current: &VecModel<MessageRow>, next: Vec<MessageRow>) -> bool {
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

    let disclosure_changed = next
        .iter()
        .take(shared_len)
        .enumerate()
        .any(|(index, row)| {
            current.row_data(index).is_some_and(|current| {
                current.collapsed != row.collapsed
                    || current.response_summary != row.response_summary
                    || current.response_id != row.response_id
            })
        });
    if disclosure_changed {
        // Slint's ListView can retain a stale delegate height when a row is
        // expanded in place. Replacing the vector forces a structural reflow.
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

pub(super) fn message_rows_match(current: &MessageRow, next: &MessageRow) -> bool {
    current.id == next.id
        && current.kind == next.kind
        && current.title == next.title
        && current.body == next.body
        && current.status == next.status
        && current.user == next.user
        && current.activity == next.activity
        && current.collapsed == next.collapsed
        && current.response_summary == next.response_summary
        && current.response_id == next.response_id
        && current.duration_label == next.duration_label
        && current.row_height == next.row_height
        && current.scroll_offset == next.scroll_offset
}

fn message_scroll_height(row: &MessageRow) -> f32 {
    if row.activity && !row.collapsed && !row.body.is_empty() {
        // Expanded activity delegates use rendered text metrics. This
        // estimate covers explicit lines and wrapping without forcing
        // the ListView to instantiate every off-screen delegate.
        let characters_per_line = if row.kind == "command" { 105 } else { 112 };
        wrapped_line_count(&row.body, characters_per_line) as f32 * 15.0 + 44.0
    } else {
        row.row_height
    }
}

pub(super) fn message_content_height(rows: &[MessageRow]) -> f32 {
    rows.iter().map(message_scroll_height).sum()
}

pub(super) fn message_rows(items: &[ConversationItem]) -> Vec<MessageRow> {
    let mut rows = Vec::new();
    let mut index = 0;
    while index < items.len() {
        if items[index].kind != ItemKind::User {
            if message_is_visible(&items[index]) {
                rows.push(message_row(&items[index]));
            }
            index += 1;
            continue;
        }

        rows.push(message_row(&items[index]));
        let response_start = index + 1;
        let response_end = items[response_start..]
            .iter()
            .position(|item| item.kind == ItemKind::User)
            .map(|offset| response_start + offset)
            .unwrap_or(items.len());
        let final_answer = items[response_start..response_end]
            .iter()
            .rposition(|item| item.kind == ItemKind::Assistant && item.duration_ms.is_some())
            .map(|offset| response_start + offset);

        if let Some(final_answer) = final_answer {
            let has_details = (response_start..response_end)
                .any(|detail| detail != final_answer && message_is_visible(&items[detail]));
            if has_details {
                rows.push(response_summary_row(&items[final_answer]));
                if !items[final_answer].response_details_collapsed {
                    rows.extend(
                        (response_start..response_end)
                            .filter(|detail| {
                                *detail != final_answer && message_is_visible(&items[*detail])
                            })
                            .map(|detail| message_row(&items[detail])),
                    );
                }
                rows.push(message_row(&items[final_answer]));
            } else {
                rows.extend(
                    items[response_start..response_end]
                        .iter()
                        .filter(|item| message_is_visible(item))
                        .map(message_row),
                );
            }
        } else {
            rows.extend(
                items[response_start..response_end]
                    .iter()
                    .filter(|item| message_is_visible(item))
                    .map(message_row),
            );
        }
        index = response_end;
    }
    let mut scroll_offset = 0.0;
    for row in &mut rows {
        row.scroll_offset = scroll_offset;
        scroll_offset += message_scroll_height(row);
    }
    rows
}

fn message_is_visible(item: &ConversationItem) -> bool {
    !(item.kind == ItemKind::Reasoning && item.body.trim().is_empty() && item.status != "running")
}

fn message_row(item: &ConversationItem) -> MessageRow {
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
        collapsed: item.collapsed,
        response_summary: false,
        response_id: "".into(),
        duration_label: item
            .duration_ms
            .map(elapsed_duration_label)
            .unwrap_or_default()
            .into(),
        row_height,
        scroll_offset: 0.0,
    }
}

fn response_summary_row(final_answer: &ConversationItem) -> MessageRow {
    let duration_label = final_answer
        .duration_ms
        .map(elapsed_duration_label)
        .unwrap_or_default();
    MessageRow {
        id: format!("response-details-{}", final_answer.id).into(),
        kind: "response-summary".into(),
        title: format!("Worked for {duration_label}").into(),
        body: "".into(),
        markdown_blocks: model(Vec::<MarkdownBlock>::new()),
        status: "completed".into(),
        user: false,
        activity: false,
        collapsed: final_answer.response_details_collapsed,
        response_summary: true,
        response_id: final_answer.id.clone().into(),
        duration_label: duration_label.into(),
        row_height: 48.0,
        scroll_offset: 0.0,
    }
}

pub(super) fn question_row(index: usize, question: &Question) -> QuestionRow {
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

pub(super) fn activity_title(item: &ConversationItem) -> String {
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

pub(super) fn model<T: Clone + 'static>(values: impl IntoIterator<Item = T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(values.into_iter().collect::<Vec<_>>()))
}
