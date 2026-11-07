//! Presentation state is independent of the model session and its durable ledger.
use crate::display;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
fn safe_multiline(text: &str) -> String {
    text.replace("\r\n", "\n")
        .split('\n')
        .map(|line| display::safe(&line.replace('\t', "    ")))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Debug)]
enum Entry {
    Text(String),
    Tool {
        id: String,
        name: String,
        state: String,
        detail: String,
    },
}
#[derive(Default)]
pub(super) struct Transcript {
    entries: Vec<Entry>,
    pub expanded: bool,
    pub search: String,
    pub top: Option<usize>,
    pub height: usize,
    pub rows: usize,
    revision: u64,
    cache_key: Option<(u64, u16, bool, String, String)>,
    cache: Vec<Line<'static>>,
}
impl Transcript {
    pub fn push(&mut self, text: &str) {
        if !text.is_empty() {
            self.entries.push(Entry::Text(safe_multiline(text)));
            self.revision += 1;
        }
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn append_agent(&mut self, stream: u64, text: &str) {
        let id = format!("agent:{stream}");
        if let Some(Entry::Tool { detail, .. }) = self
            .entries
            .iter_mut()
            .find(|e| matches!(e, Entry::Tool { id: old, .. } if old == &id))
        {
            detail.push_str(&safe_multiline(text));
            self.revision += 1;
        } else {
            self.tool(&id, "Uzman çıktısı", "çalışıyor", text);
        }
    }
    pub fn finish_running(&mut self, state: &str) {
        for entry in &mut self.entries {
            if let Entry::Tool { state: current, .. } = entry
                && current == "çalışıyor"
            {
                *current = state.into();
            }
        }
        self.revision += 1;
    }
    pub fn tool(&mut self, id: &str, name: &str, state: &str, detail: &str) {
        let label = match name {
            "read_file" | "fs_read" | "fs_read_page" => "Dosya okunuyor",
            "list_files" | "fs_list" => "Dosyalar listeleniyor",
            "search_code" | "code_search" => "Kod aranıyor",
            "propose_work" => "Çalışma kapsamı",
            "request_workspace" => "Çalışma klasörü",
            "memory_recall" => "Proje belleği",
            "conversation_history" => "Önceki konuşma",
            "fs_create" => "Dosya oluşturma",
            "fs_patch" => "Dosya düzenleme",
            "fs_move" => "Dosya taşıma",
            "fs_delete" => "Dosya silme",
            "fs_mkdir" => "Klasör oluşturma",
            "process_exec" | "container_exec" => "Komut çalıştırma",
            "code_tests" | "sandbox_tests" => "Testler",
            "code_build" | "sandbox_build" => "Derleme",
            _ => name,
        };
        let entry = Entry::Tool {
            id: id.into(),
            name: display::safe(label),
            state: state.into(),
            detail: safe_multiline(detail),
        };
        if let Some(old) = self
            .entries
            .iter_mut()
            .find(|e| matches!(e, Entry::Tool { id: old, .. } if old == id))
        {
            *old = entry;
        } else {
            self.entries.push(entry);
        }
        self.revision += 1;
    }
    pub fn scroll(&mut self, delta: isize) {
        let end = self.rows.saturating_sub(self.height);
        let next = self
            .top
            .unwrap_or(end)
            .saturating_add_signed(delta)
            .min(end);
        self.top = if next == end { None } else { Some(next) };
    }
    pub fn bottom(&mut self) {
        self.top = None;
    }
    pub fn plain(&self) -> String {
        self.entries
            .iter()
            .map(|e| match e {
                Entry::Text(s) => s.clone(),
                Entry::Tool {
                    name,
                    state,
                    detail,
                    ..
                } => format!("{name} · {state}\n{detail}"),
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
    pub fn find_next(&mut self) -> bool {
        if self.search.is_empty() {
            return false;
        }
        let query = self.search.to_lowercase();
        let start = self.top.map_or(0, |n| n.saturating_add(1));
        let found = (start..self.cache.len())
            .chain(0..start.min(self.cache.len()))
            .find(|&n| {
                self.cache[n]
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .to_lowercase()
                    .contains(&query)
            });
        if let Some(n) = found {
            self.top = Some(n.min(self.rows.saturating_sub(self.height)));
            true
        } else {
            false
        }
    }
    pub fn lines(&mut self, width: u16, live: &str, accent: Color) -> &[Line<'static>] {
        let valid =
            self.cache_key
                .as_ref()
                .is_some_and(|(revision, columns, expanded, search, last)| {
                    *revision == self.revision
                        && *columns == width
                        && *expanded == self.expanded
                        && search == &self.search
                        && last == live
                });
        if !valid {
            let mut out = Vec::new();
            for entry in &self.entries {
                match entry {
                    Entry::Text(text) => out.extend(markdown(text, width, accent)),
                    Entry::Tool {
                        name,
                        state,
                        detail,
                        ..
                    } => {
                        let color = match state.as_str() {
                            "tamamlandı" => Color::Green,
                            "reddedildi" | "hata" => Color::Yellow,
                            _ => accent,
                        };
                        out.extend(wrap(
                            &format!(
                                "  {} {name} · {state}",
                                if self.expanded { "▾" } else { "▸" }
                            ),
                            width,
                            Style::default().fg(color),
                        ));
                        if self.expanded {
                            out.extend(markdown(detail, width, accent));
                        }
                    }
                }
                out.push(Line::default());
            }
            if !live.is_empty() {
                out.extend(markdown(live, width, accent));
            }
            if !self.search.is_empty() {
                let query = self.search.to_lowercase();
                for line in &mut out {
                    if line
                        .spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                        .to_lowercase()
                        .contains(&query)
                    {
                        line.style = Style::default().bg(Color::Rgb(65, 53, 26));
                    }
                }
            }
            self.cache = out;
            self.cache_key = Some((
                self.revision,
                width,
                self.expanded,
                self.search.clone(),
                live.into(),
            ));
        }
        self.rows = self.cache.len();
        &self.cache
    }
}
fn inline(text: &str, base: Style, accent: Color) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let next = rest
            .find("**")
            .map(|n| (n, "**"))
            .into_iter()
            .chain(rest.find('`').map(|n| (n, "`")))
            .min_by_key(|(n, _)| *n);
        let Some((at, delimiter)) = next else {
            spans.push(Span::styled(rest.to_owned(), base));
            break;
        };
        if at > 0 {
            spans.push(Span::styled(rest[..at].to_owned(), base));
        }
        let tail = &rest[at + delimiter.len()..];
        if let Some(end) = tail.find(delimiter) {
            let style = if delimiter == "**" {
                base.add_modifier(Modifier::BOLD)
            } else {
                base.fg(accent)
            };
            spans.push(Span::styled(tail[..end].to_owned(), style));
            rest = &tail[end + delimiter.len()..];
        } else {
            spans.push(Span::styled(rest[at..].to_owned(), base));
            break;
        }
    }
    spans
}
fn wrap_spans(spans: Vec<Span<'static>>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    let mut row = Vec::new();
    let mut used = 0;
    for span in spans {
        let mut part = String::new();
        for g in span.content.graphemes(true) {
            let size = g.width();
            if used > 0 && used + size > width {
                row.push(Span::styled(std::mem::take(&mut part), span.style));
                lines.push(Line::from(std::mem::take(&mut row)));
                used = 0;
            }
            part.push_str(g);
            used += size;
        }
        if !part.is_empty() {
            row.push(Span::styled(part, span.style));
        }
    }
    lines.push(Line::from(row));
    lines
}
pub(super) fn wrap(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    wrap_spans(vec![Span::styled(text.to_owned(), style)], width)
}
pub(super) fn markdown(text: &str, width: u16, accent: Color) -> Vec<Line<'static>> {
    let mut code = false;
    let mut out = Vec::new();
    for line in safe_multiline(text).lines() {
        if let Some(lang) = line.trim_start().strip_prefix("```") {
            code = !code;
            out.extend(wrap(
                &format!("  {}", if code { lang.trim() } else { "─" }),
                width,
                Style::default().fg(Color::DarkGray),
            ));
        } else if code {
            let color = if line.starts_with('+') {
                Color::Green
            } else if line.starts_with('-') {
                Color::Red
            } else {
                Color::Gray
            };
            out.extend(wrap(line, width, Style::default().fg(color)));
        } else if line.starts_with("❯ ") {
            out.extend(wrap(
                line,
                width,
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ));
        } else if line.starts_with('#') && line.contains(' ') {
            out.extend(wrap(
                line.trim_start_matches('#').trim_start(),
                width,
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ));
        } else {
            out.extend(wrap_spans(inline(line, Style::default(), accent), width));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiline_text_preserves_layout_without_terminal_escape_injection() {
        let rows = markdown("bir\nikinci\r\n\tüçüncü\n\x1b[2J", 80, Color::Cyan);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1].spans[0].content, "ikinci");
        assert!(rows[2].spans[0].content.starts_with("    üçüncü"));
        assert!(!rows[3].spans[0].content.contains('\x1b'));
        let mut t = Transcript::default();
        t.push("bir\niki");
        assert_eq!(t.plain(), "bir\niki");
    }
    #[test]
    fn browsing_history_does_not_follow_new_output() {
        let mut t = Transcript::default();
        t.push(&"satır\n".repeat(50));
        t.height = 8;
        t.lines(30, "", Color::Cyan);
        t.scroll(-12);
        let anchor = t.top;
        t.push("yeni yanıt");
        t.lines(30, "", Color::Cyan);
        assert_eq!(t.top, anchor);
        t.bottom();
        assert!(t.top.is_none());
    }
    #[test]
    fn tools_update_in_place_and_search_crosses_messages() {
        let mut t = Transcript::default();
        t.tool("1", "Dosya oku", "çalışıyor", "");
        t.tool("1", "Dosya oku", "tamamlandı", "src/main.rs");
        t.push("Merhaba dünya");
        assert_eq!(t.entries.len(), 2);
        t.search = "dünya".into();
        t.height = 2;
        t.lines(40, "", Color::Cyan);
        assert!(t.find_next());
        assert!(!t.plain().contains("çalışıyor"));
    }
    #[test]
    fn unicode_markdown_and_code_survive_narrow_layout() {
        let rows = markdown(
            "**Türkçe** `kod`\n```rust\n+🦀 merhaba\n```",
            8,
            Color::Cyan,
        );
        assert!(rows.iter().all(|r| r.width() <= 8));
        let text = rows
            .iter()
            .flat_map(|r| &r.spans)
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(text.contains("Türkçe"));
        assert!(text.contains('🦀'));
    }
}
