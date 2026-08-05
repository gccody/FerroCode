use crate::{ConversationItem, ItemKind, ModelOption};

pub fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    format!("{}…", text.chars().take(max_chars).collect::<String>())
}

pub fn short_path(path: &str, max_chars: usize) -> String {
    if path.chars().count() <= max_chars {
        return path.to_owned();
    }
    let keep = max_chars.saturating_sub(1);
    let tail = path
        .chars()
        .rev()
        .take(keep)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("…{tail}")
}

pub fn humanize(value: &str) -> String {
    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index > 0 && ch.is_uppercase() {
            output.push(' ');
        }
        if index == 0 {
            output.extend(ch.to_uppercase());
        } else {
            output.push(ch);
        }
    }
    output.replace(['_', '-'], " ")
}

pub fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

pub fn model_display(models: &[ModelOption], id: &str) -> String {
    models
        .iter()
        .find(|model| model.id == id)
        .map(|model| model.display_name.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if id.is_empty() {
                "Default model".into()
            } else {
                id.to_owned()
            }
        })
}

pub fn fallback_activity_summary(items: &[ConversationItem]) -> String {
    let count = items.len();
    let label = match items.first().map(|item| item.kind) {
        Some(ItemKind::Command) => "Ran commands",
        Some(ItemKind::FileChange) => "Changed files",
        Some(ItemKind::Plan) => "Updated plan",
        Some(ItemKind::Tool) => "Used tools",
        _ => "Completed actions",
    };
    format!("{label} · {count}")
}

pub fn is_skills_budget_warning(message: &str) -> bool {
    message.contains("skills context budget")
        || message.contains("Skill descriptions were shortened")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_is_unicode_safe() {
        assert_eq!(truncate_text("a🙂bc", 3), "a🙂b…");
        assert_eq!(short_path("abcdef", 4), "…def");
    }

    #[test]
    fn formats_tokens_and_protocol_names() {
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(12_345), "12.3K");
        assert_eq!(humanize("fileChange"), "File Change");
    }
}
