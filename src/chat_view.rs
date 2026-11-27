use egui::text::LayoutJob;
use egui::{Color32, FontId, TextFormat, Visuals};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

/// Accent used for user bubbles and chat highlights.
pub const CHAT_ACCENT: Color32 = Color32::from_rgb(116, 93, 215);

fn normal_format(visuals: &Visuals) -> TextFormat {
    TextFormat {
        font_id: FontId::proportional(14.0),
        color: visuals.text_color(),
        ..Default::default()
    }
}

fn code_format(visuals: &Visuals) -> TextFormat {
    TextFormat {
        font_id: FontId::monospace(12.0),
        color: visuals.text_color(),
        background: visuals.widgets.noninteractive.weak_bg_fill,
        ..Default::default()
    }
}

fn push(job: &mut LayoutJob, text: &str, format: &TextFormat) {
    if !text.is_empty() {
        job.append(text, 0.0, format.clone());
    }
}

fn end_block(job: &mut LayoutJob, format: &TextFormat) {
    if !job.text.is_empty() && !job.text.ends_with('\n') {
        job.append("\n", 0.0, format.clone());
    }
}

/// Converts the small markdown subset the assistant is asked to produce into a
/// single [`LayoutJob`]: prose keeps the visuals' text color, while inline and
/// fenced code use a monospace font on the weak background fill.
pub fn markdown_layout(text: &str, visuals: &Visuals) -> LayoutJob {
    let normal = normal_format(visuals);
    let code = code_format(visuals);
    let mut job = LayoutJob::default();
    let mut in_code_block = false;

    for event in Parser::new(text) {
        match event {
            Event::Text(text) => {
                if in_code_block {
                    push(&mut job, &text, &code);
                } else {
                    push(&mut job, &text, &normal);
                }
            }
            Event::Code(text) => push(&mut job, &text, &code),
            Event::Start(Tag::CodeBlock(_)) => {
                end_block(&mut job, &normal);
                in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                end_block(&mut job, &normal);
            }
            Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::Heading(_))
            | Event::End(TagEnd::Item)
            | Event::End(TagEnd::List(_))
            | Event::End(TagEnd::BlockQuote(_)) => end_block(&mut job, &normal),
            Event::Start(Tag::Item) => push(&mut job, "• ", &normal),
            Event::SoftBreak | Event::HardBreak => push(&mut job, "\n", &normal),
            Event::Rule => end_block(&mut job, &normal),
            _ => {}
        }
    }

    job
}

/// One-line summary of a tool result, used by the chat transcript.
///
/// The full text still goes to the model; this is only what the user sees
/// before expanding the message.
pub fn tool_result_summary(content: &str) -> String {
    let first = content.lines().next().unwrap_or("").trim();
    let summary = if first.is_empty() {
        "tool result"
    } else {
        first
    };
    summary.chars().take(100).collect()
}

/// Returns the fenced code blocks in `text`, trimmed of their trailing
/// newline, so the UI can offer "insert" and "execute" actions per block.
pub fn code_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut in_code_block = false;

    for event in Parser::new(text) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                current.clear();
                in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                blocks.push(current.trim_end_matches('\n').to_owned());
            }
            Event::Text(text) if in_code_block => current.push_str(&text),
            _ => {}
        }
    }

    blocks
}

/// True when a code block is a single non-empty line, safe to send to the PTY
/// as one command. Multi-line blocks stay insert-only.
pub fn is_single_line_command(block: &str) -> bool {
    !block.trim().is_empty() && !block.contains(['\n', '\r'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_layout_keeps_inline_and_fenced_code_as_monospace() {
        let job = markdown_layout("texto `a` y\n```\ngit status\n```", &Visuals::dark());
        assert!(job.text.contains("git status"));
        assert!(job
            .sections
            .iter()
            .any(|s| s.format.font_id == FontId::monospace(12.0)));
    }

    #[test]
    fn code_blocks_returns_fenced_content_without_trailing_newline() {
        let blocks = code_blocks("before\n```bash\ngit status\n```\nafter");
        assert_eq!(blocks, vec!["git status".to_owned()]);
    }

    #[test]
    fn single_line_command_accepts_one_non_empty_line() {
        assert!(is_single_line_command("cargo test"));
        assert!(is_single_line_command("  git status  "));
    }

    #[test]
    fn single_line_command_rejects_empty_and_multiline_blocks() {
        assert!(!is_single_line_command(""));
        assert!(!is_single_line_command("   \n  "));
        assert!(!is_single_line_command("git status\ngit diff"));
        assert!(!is_single_line_command("git status\rgit diff"));
    }

    #[test]
    fn tool_result_summary_is_one_line() {
        let summary = tool_result_summary("list_dir: 42 entries\na.txt\nb.txt");
        assert_eq!(summary, "list_dir: 42 entries");
    }
}
