use super::*;
use codeagent_app::{AppState, Question};
use codeagent_core::{ConversationItem, ItemKind};
use slint::{Model, StyledText, VecModel};
use std::collections::HashSet;

#[test]
fn change_rows_turn_git_status_into_readable_file_metadata() {
    let rows = change_rows(
        " M crates/app/src/main.rs\nA  docs/guide.md\n?? assets/logo.png\nR  old.txt -> src/new.txt\nUU conflicted.rs\n",
    );

    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].row.name.as_str(), "main.rs");
    assert_eq!(rows[0].row.detail.as_str(), "Modified · crates/app/src");
    assert_eq!(rows[0].row.state_label.as_str(), "UNSTAGED");
    assert!(!rows[0].staged);
    assert!(rows[0].unstaged);

    assert_eq!(rows[1].row.status.as_str(), "A");
    assert_eq!(rows[1].row.state_label.as_str(), "STAGED");
    assert!(rows[1].staged);
    assert!(!rows[1].unstaged);

    assert_eq!(rows[2].row.status_label.as_str(), "Untracked");
    assert_eq!(rows[3].row.name.as_str(), "new.txt");
    assert_eq!(
        rows[3].row.detail.as_str(),
        "Renamed · old.txt -> src/new.txt"
    );
    assert_eq!(rows[3].path, "src/new.txt");
    assert_eq!(rows[4].row.status.as_str(), "!");
    assert_eq!(rows[4].row.status_label.as_str(), "Conflict");
}

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
fn elapsed_duration_labels_are_compact_and_readable() {
    assert_eq!(elapsed_duration_label(999), "0s");
    assert_eq!(elapsed_duration_label(42_000), "42s");
    assert_eq!(elapsed_duration_label(68_000), "1m 08s");
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
fn completed_assistant_message_height_includes_the_copy_action() {
    let mut answer = ConversationItem::new("answer", ItemKind::Assistant, "Codex");
    answer.body = "x".repeat(113);
    let streaming_height = message_height(&answer, &markdown_blocks(&answer));

    answer.duration_ms = Some(1_000);
    let completed_height = message_height(&answer, &markdown_blocks(&answer));

    assert_eq!(completed_height - streaming_height, 35.0);
}

#[test]
fn activity_items_use_compact_rows_and_past_tense_when_finished() {
    let mut command = ConversationItem::new("command", ItemKind::Command, "cargo test");
    command.status = "completed".into();

    assert_eq!(message_height(&command, &[]), 42.0);
    assert_eq!(activity_title(&command), "Ran command");
}

#[test]
fn completed_responses_collapse_into_one_summary_above_the_final_answer() {
    let user = ConversationItem::new("user", ItemKind::User, "You");
    let commentary = ConversationItem::new("commentary", ItemKind::Assistant, "Codex");
    let mut tool = ConversationItem::new("tool", ItemKind::Tool, "Search");
    tool.collapsed = true;
    let mut final_answer = ConversationItem::new("final", ItemKind::Assistant, "Codex");
    final_answer.body = "Done".into();
    final_answer.duration_ms = Some(321_000);
    final_answer.response_details_collapsed = true;

    let collapsed = message_rows(&[
        user.clone(),
        commentary.clone(),
        tool.clone(),
        final_answer.clone(),
    ]);

    assert_eq!(collapsed.len(), 3);
    assert!(collapsed[1].response_summary);
    assert_eq!(collapsed[1].title.as_str(), "Worked for 5m 21s");
    assert!(collapsed[1].collapsed);
    assert_eq!(collapsed[2].id.as_str(), "final");

    final_answer.response_details_collapsed = false;
    let expanded = message_rows(&[user, commentary, tool, final_answer]);
    assert_eq!(expanded.len(), 5);
    assert!(!expanded[1].collapsed);
    assert_eq!(expanded[2].id.as_str(), "commentary");
    assert_eq!(expanded[3].id.as_str(), "tool");
    assert_eq!(expanded[4].id.as_str(), "final");
}

#[test]
fn expanding_a_nested_activity_preserves_every_response_row() {
    let user = ConversationItem::new("user", ItemKind::User, "You");
    let commentary = ConversationItem::new("commentary", ItemKind::Assistant, "Codex");
    let mut command = ConversationItem::new("command", ItemKind::Command, "cargo test");
    command.body = "test output ".repeat(120);
    command.collapsed = true;
    let mut final_answer = ConversationItem::new("final", ItemKind::Assistant, "Codex");
    final_answer.body = "Done".into();
    final_answer.duration_ms = Some(2_000);

    let items = [user, commentary, command.clone(), final_answer];
    let current = VecModel::from(message_rows(&items));

    command.collapsed = false;
    let expanded_items = [
        items[0].clone(),
        items[1].clone(),
        command,
        items[3].clone(),
    ];
    assert!(update_message_rows(&current, message_rows(&expanded_items)));

    assert_eq!(current.row_count(), 5);
    assert_eq!(
        (0..current.row_count())
            .map(|index| current.row_data(index).unwrap().id.to_string())
            .collect::<Vec<_>>(),
        [
            "user",
            "response-details-final",
            "commentary",
            "command",
            "final"
        ]
    );
    assert!(!current.row_data(3).unwrap().collapsed);
    assert_eq!(current.row_data(3).unwrap().body, expanded_items[2].body);
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
    assert!(!rows[0].age_label.is_empty());
    assert!(rows[0].active);
    assert!(rows[0].busy);
    assert_eq!(rows[0].project_index, 0);
    assert_eq!(rows[1].project_id.as_str(), first_project);
    assert_eq!(rows[1].subtitle.as_str(), "1 message");
    assert!(rows[1].completed_unread);
    assert_eq!(rows[1].project_index, 0);

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
fn relative_thread_ages_use_compact_sidebar_labels() {
    assert_eq!(relative_time_label(1_000, 1_000), "now");
    assert_eq!(relative_time_label(1_000, 1_059), "now");
    assert_eq!(relative_time_label(1_000, 1_060), "1m ago");
    assert_eq!(relative_time_label(1_000, 8_200), "2h ago");
    assert_eq!(relative_time_label(1_000, 260_200), "3d ago");
    assert_eq!(relative_time_label(2_000, 1_000), "now");
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
    assert_eq!(
        blocks[0].raw_text.as_str(),
        "Summary\n- first\n  * nested\n1. ordered\ninline code"
    );
    assert!(markdown_blocks(&user).is_empty());
}

#[test]
fn selectable_markdown_text_omits_formatting_and_link_destinations() {
    assert_eq!(
        markdown_plain_text("**Bold** [nested *label*](https://example.com) and 2 * 3"),
        "Bold nested label and 2 * 3"
    );
    assert_eq!(
        markdown_plain_text("file_name and ~~removed~~"),
        "file_name and removed"
    );
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
    assert_eq!(
        table
            .table_rows
            .row_data(1)
            .unwrap()
            .cells
            .row_data(1)
            .unwrap()
            .raw_text
            .as_str(),
        "Active"
    );
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
    assert!((table.table_width - (name_width + status_width + score_width)).abs() < f32::EPSILON);
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
