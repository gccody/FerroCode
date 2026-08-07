use crate::{
    MainWindow, MarkdownBlock, MessageRow, ProjectRow, QuestionRow, ThreadRow, markdown_blocks,
    wrapped_line_count,
};
use codeagent_app::{AppState, Question};
use codeagent_core::{ConversationItem, ItemKind};
use slint::{Model, ModelRc, VecModel};
use std::collections::BTreeMap;

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

pub(super) fn thread_rows(state: &AppState, search: &str) -> Vec<ThreadRow> {
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
        && current.row_height == next.row_height
}

pub(super) fn message_rows(items: &[ConversationItem]) -> Vec<MessageRow> {
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
