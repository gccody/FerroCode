use crate::{MarkdownBlock, MarkdownTableCell, MarkdownTableRow, model};
use codeagent_core::{ConversationItem, ItemKind};
use slint::StyledText;

pub(super) fn wrapped_line_count(text: &str, max_chars: usize) -> usize {
    text.lines()
        .map(|line| line.chars().count().max(1).div_ceil(max_chars))
        .sum::<usize>()
        .max(1)
}

pub(super) fn markdown_blocks(item: &ConversationItem) -> Vec<MarkdownBlock> {
    if !matches!(item.kind, ItemKind::Assistant | ItemKind::Reasoning) {
        return Vec::new();
    }

    parse_markdown_blocks(&item.body, item.kind == ItemKind::Reasoning)
}

pub(super) fn parse_markdown_blocks(markdown: &str, reasoning: bool) -> Vec<MarkdownBlock> {
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut blocks = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        if lines[index].trim().is_empty() {
            index += 1;
            continue;
        }

        if let Some((marker, count, language)) = opening_fence(lines[index]) {
            index += 1;
            let mut code = Vec::new();
            while index < lines.len() && !closing_fence(lines[index], marker, count) {
                code.push(lines[index]);
                index += 1;
            }
            if index < lines.len() {
                index += 1;
            }
            blocks.push(code_block(&code.join("\n"), language));
            continue;
        }

        if is_indented_code(lines[index]) {
            let mut code = Vec::new();
            while index < lines.len()
                && (is_indented_code(lines[index]) || lines[index].trim().is_empty())
            {
                code.push(
                    lines[index]
                        .strip_prefix("    ")
                        .or_else(|| lines[index].strip_prefix('\t'))
                        .unwrap_or_default(),
                );
                index += 1;
            }
            blocks.push(code_block(&code.join("\n"), "text"));
            continue;
        }

        if index + 1 < lines.len()
            && let Some(alignments) = table_alignments(lines[index + 1])
        {
            let headers = split_table_row(lines[index]);
            if headers.len() == alignments.len() && headers.len() > 1 {
                index += 2;
                let mut source_rows = vec![(headers, true)];
                while index < lines.len() && !lines[index].trim().is_empty() {
                    let cells = split_table_row(lines[index]);
                    if cells.len() != alignments.len() {
                        break;
                    }
                    source_rows.push((cells, false));
                    index += 1;
                }
                let column_widths = table_column_widths(&source_rows, alignments.len());
                let rows = source_rows
                    .iter()
                    .map(|(cells, header)| table_row(cells, &alignments, &column_widths, *header))
                    .collect();
                blocks.push(table_block(rows, &column_widths));
                continue;
            }
        }

        if let Some((level, heading)) = atx_heading(lines[index]) {
            blocks.push(heading_block(level, heading));
            index += 1;
            continue;
        }

        if markdown_quote(lines[index]).is_some() {
            let mut quote = Vec::new();
            while index < lines.len() {
                let Some(content) = markdown_quote(lines[index]) else {
                    break;
                };
                quote.push(content);
                index += 1;
            }
            blocks.push(styled_block("quote", &quote.join("\n"), reasoning));
            continue;
        }

        if index + 1 < lines.len()
            && let Some(level) = setext_heading_level(lines[index + 1])
        {
            blocks.push(heading_block(level, lines[index].trim()));
            index += 2;
            continue;
        }

        if is_markdown_rule(lines[index].trim()) {
            blocks.push(MarkdownBlock {
                kind: "rule".into(),
                block_height: 9.0,
                ..empty_markdown_block()
            });
            index += 1;
            continue;
        }

        let mut paragraph = Vec::new();
        while index < lines.len() && !lines[index].trim().is_empty() {
            if !paragraph.is_empty() && starts_block(&lines, index) {
                break;
            }
            paragraph.push(lines[index]);
            index += 1;
        }
        blocks.push(styled_block("paragraph", &paragraph.join("\n"), reasoning));
    }

    if blocks.is_empty() {
        blocks.push(styled_block("paragraph", "", reasoning));
    }
    blocks
}

pub(super) fn starts_block(lines: &[&str], index: usize) -> bool {
    opening_fence(lines[index]).is_some()
        || is_indented_code(lines[index])
        || atx_heading(lines[index]).is_some()
        || markdown_quote(lines[index]).is_some()
        || is_markdown_rule(lines[index].trim())
        || (index + 1 < lines.len()
            && (setext_heading_level(lines[index + 1]).is_some()
                || table_alignments(lines[index + 1]).is_some()))
}

pub(super) fn empty_markdown_block() -> MarkdownBlock {
    MarkdownBlock {
        kind: "".into(),
        text: StyledText::default(),
        raw_text: "".into(),
        language: "".into(),
        level: 0,
        block_height: 0.0,
        column_count: 0,
        table_width: 0.0,
        table_height: 0.0,
        table_rows: model(Vec::<MarkdownTableRow>::new()),
    }
}

pub(super) fn styled_block(kind: &str, markdown: &str, reasoning: bool) -> MarkdownBlock {
    let normalized = normalize_inline_markdown(markdown);
    let text = StyledText::from_markdown(&normalized)
        .unwrap_or_else(|_| StyledText::from_plain_text(markdown));
    let line_height = if reasoning { 15.0 } else { 16.0 };
    let height = wrapped_line_count(markdown, 112) as f32 * line_height;
    // StyledText can paint Segoe UI descenders slightly below the estimated
    // line box. Leave a little vertical room so letters such as g, p, and y
    // are not clipped at the bottom of an assistant message.
    const GLYPH_OVERFLOW: f32 = 2.0;
    MarkdownBlock {
        kind: kind.into(),
        text,
        block_height: if kind == "quote" {
            height + 14.0 + GLYPH_OVERFLOW
        } else {
            height.max(line_height) + GLYPH_OVERFLOW
        },
        ..empty_markdown_block()
    }
}

pub(super) fn heading_block(level: i32, markdown: &str) -> MarkdownBlock {
    let normalized = normalize_inline_markdown(markdown);
    let text = StyledText::from_markdown(&normalized)
        .unwrap_or_else(|_| StyledText::from_plain_text(markdown));
    let height = match level {
        1 => 34.0,
        2 => 30.0,
        3 => 27.0,
        4 => 24.0,
        _ => 22.0,
    };
    MarkdownBlock {
        kind: "heading".into(),
        text,
        level,
        block_height: height,
        ..empty_markdown_block()
    }
}

pub(super) fn code_block(code: &str, language: &str) -> MarkdownBlock {
    MarkdownBlock {
        kind: "code".into(),
        raw_text: code.into(),
        language: if language.is_empty() {
            "text"
        } else {
            language
        }
        .into(),
        block_height: wrapped_line_count(code, 105) as f32 * 16.0 + 42.0,
        ..empty_markdown_block()
    }
}

pub(super) fn table_block(rows: Vec<MarkdownTableRow>, column_widths: &[f32]) -> MarkdownBlock {
    let height = rows.iter().map(|row| row.row_height).sum::<f32>();
    MarkdownBlock {
        kind: "table".into(),
        block_height: height + 12.0,
        column_count: column_widths.len().min(i32::MAX as usize) as i32,
        table_width: column_widths.iter().sum(),
        table_height: height,
        table_rows: model(rows),
        ..empty_markdown_block()
    }
}

pub(super) fn table_column_widths(rows: &[(Vec<String>, bool)], column_count: usize) -> Vec<f32> {
    let mut widths = vec![48.0_f32; column_count];
    for (cells, _) in rows {
        for (index, cell) in cells.iter().enumerate() {
            let longest_line = cell
                .lines()
                .map(|line| markdown_visible_len(line.trim()))
                .max()
                .unwrap_or_default();
            widths[index] = widths[index].max((longest_line as f32 * 6.8 + 18.0).min(420.0));
        }
    }
    widths
}

pub(super) fn table_row(
    cells: &[String],
    alignments: &[&str],
    column_widths: &[f32],
    header: bool,
) -> MarkdownTableRow {
    let lines = cells
        .iter()
        .zip(column_widths)
        .map(|(cell, width)| {
            let chars_per_line = (((*width - 18.0) / 6.8).round() as usize).max(1);
            cell.lines()
                .map(|line| {
                    markdown_visible_len(line.trim())
                        .max(1)
                        .div_ceil(chars_per_line)
                })
                .sum::<usize>()
                .max(1)
        })
        .max()
        .unwrap_or(1);
    MarkdownTableRow {
        cells: model(cells.iter().zip(alignments).zip(column_widths).map(
            |((cell, alignment), width)| {
                let normalized = normalize_inline_markdown(cell.trim());
                MarkdownTableCell {
                    text: StyledText::from_markdown(&normalized)
                        .unwrap_or_else(|_| StyledText::from_plain_text(cell.trim())),
                    alignment: (*alignment).into(),
                    column_width: *width,
                }
            },
        )),
        header,
        row_height: (lines as f32 * 16.0 + 12.0).max(30.0),
    }
}

pub(super) fn markdown_visible_len(markdown: &str) -> usize {
    let mut count = 0;
    let mut link_destination = false;
    let mut chars = markdown.chars().peekable();
    while let Some(ch) = chars.next() {
        if link_destination {
            if ch == ')' {
                link_destination = false;
            }
            continue;
        }
        if ch == ']' && chars.peek() == Some(&'(') {
            chars.next();
            link_destination = true;
            continue;
        }
        if matches!(ch, '*' | '_' | '~' | '`' | '[' | ']') {
            continue;
        }
        if ch == '\\' {
            if chars.next().is_some() {
                count += 1;
            }
            continue;
        }
        count += 1;
    }
    count
}

pub(super) fn opening_fence(line: &str) -> Option<(char, usize, &str)> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let count = trimmed.chars().take_while(|ch| *ch == marker).count();
    if count < 3 {
        return None;
    }
    let info = trimmed[count..].trim();
    if marker == '`' && info.contains('`') {
        return None;
    }
    let language = info.split_whitespace().next().unwrap_or_default();
    Some((marker, count, language))
}

pub(super) fn closing_fence(line: &str, marker: char, count: usize) -> bool {
    let trimmed = line.trim();
    let marker_count = trimmed.chars().take_while(|ch| *ch == marker).count();
    marker_count >= count && trimmed.chars().skip(marker_count).all(char::is_whitespace)
}

pub(super) fn is_indented_code(line: &str) -> bool {
    line.starts_with("    ") || line.starts_with('\t')
}

pub(super) fn atx_heading(line: &str) -> Option<(i32, &str)> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let count = trimmed.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&count) {
        return None;
    }
    let heading = trimmed.get(count..)?.strip_prefix([' ', '\t'])?;
    Some((count as i32, heading.trim_end_matches('#').trim_end()))
}

pub(super) fn setext_heading_level(line: &str) -> Option<i32> {
    let compact = line.trim();
    if compact.is_empty() {
        return None;
    }
    if compact.chars().all(|ch| ch == '=') {
        Some(1)
    } else if compact.chars().all(|ch| ch == '-') {
        Some(2)
    } else {
        None
    }
}

pub(super) fn markdown_quote(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    trimmed
        .strip_prefix('>')
        .map(|content| content.strip_prefix(' ').unwrap_or(content))
}

pub(super) fn is_markdown_rule(line: &str) -> bool {
    let compact = line
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    compact.len() >= 3
        && compact.chars().next().is_some_and(|marker| {
            matches!(marker, '-' | '*' | '_') && compact.chars().all(|ch| ch == marker)
        })
}

pub(super) fn table_alignments(line: &str) -> Option<Vec<&'static str>> {
    let cells = split_table_row(line);
    if cells.len() < 2 {
        return None;
    }
    cells
        .iter()
        .map(|cell| {
            let cell = cell.trim();
            let core = cell.trim_matches(':');
            if core.len() < 3 || !core.chars().all(|ch| ch == '-') {
                return None;
            }
            Some(if cell.starts_with(':') && cell.ends_with(':') {
                "center"
            } else if cell.ends_with(':') {
                "right"
            } else {
                "left"
            })
        })
        .collect()
}

pub(super) fn split_table_row(line: &str) -> Vec<String> {
    let line = line.trim().trim_start_matches('|').trim_end_matches('|');
    let mut cells = vec![String::new()];
    let mut escaped = false;
    let mut code_delimiter = 0;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if escaped {
            cells.last_mut().unwrap().extend(['\\', ch]);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '`' {
            let mut run = 1;
            while chars.next_if_eq(&'`').is_some() {
                run += 1;
            }
            if code_delimiter == 0 {
                code_delimiter = run;
            } else if code_delimiter == run {
                code_delimiter = 0;
            }
            cells.last_mut().unwrap().push_str(&"`".repeat(run));
            continue;
        }
        if ch == '|' && code_delimiter == 0 {
            cells.push(String::new());
        } else {
            cells.last_mut().unwrap().push(ch);
        }
    }
    if escaped {
        cells.last_mut().unwrap().push('\\');
    }
    cells
}

pub(super) fn normalize_inline_markdown(markdown: &str) -> String {
    markdown
        .replace("- [x] ", "- â˜‘ ")
        .replace("- [X] ", "- â˜‘ ")
        .replace("- [ ] ", "- â˜ ")
        .replace("![", "[ðŸ–¼ ")
        .replace('<', "\\<")
}
