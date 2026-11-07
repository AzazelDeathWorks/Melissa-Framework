use crate::{Result, display};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use ratatui_textarea::TextArea;
use std::{io, time::Duration};
use zeroize::Zeroizing;
pub(crate) struct Ui {
    pub terminal: Terminal<CrosstermBackend<io::Stdout>>,
    pub accent: Color,
    pub muted: Color,
    pub transcript: super::transcript::Transcript,
}
fn modal_prompt(title: &str, accent: Color, width: u16) -> (Paragraph<'static>, u16) {
    let lines = super::transcript::wrap(&display::safe(title), width, Style::default().fg(accent));
    let height = lines.len().min(u16::MAX as usize) as u16;
    (Paragraph::new(lines), height)
}
fn field_prompt(
    title: &str,
    help: &str,
    error: Option<&str>,
    accent: Color,
    width: u16,
) -> (Paragraph<'static>, u16) {
    let mut lines =
        super::transcript::wrap(&display::safe(title), width, Style::default().fg(accent));
    for line in help.lines() {
        lines.extend(super::transcript::wrap(
            &display::safe(line),
            width,
            Style::default().fg(Color::DarkGray),
        ));
    }
    if let Some(error) = error {
        lines.extend(super::transcript::wrap(
            &display::safe(error),
            width,
            Style::default().fg(Color::Red),
        ));
    }
    let height = lines.len().min(u16::MAX as usize) as u16;
    (Paragraph::new(lines), height)
}
impl Drop for Ui {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}
impl Ui {
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;
        if let Err(e) = execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture
        ) {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return Err(e.into());
        }
        let colored = std::env::var_os("NO_COLOR").is_none_or(|s| s.is_empty());
        match Terminal::with_options(
            CrosstermBackend::new(io::stdout()),
            TerminalOptions {
                viewport: Viewport::Fullscreen,
            },
        ) {
            Ok(terminal) => Ok(Self {
                terminal,
                accent: if colored {
                    Color::Rgb(167, 139, 250)
                } else {
                    Color::Reset
                },
                muted: if colored {
                    Color::DarkGray
                } else {
                    Color::Reset
                },
                transcript: Default::default(),
            }),
            Err(e) => {
                let _ = disable_raw_mode();
                let _ = execute!(
                    io::stdout(),
                    DisableBracketedPaste,
                    DisableMouseCapture,
                    LeaveAlternateScreen
                );
                Err(e.into())
            }
        }
    }
    pub fn print(&mut self, text: &str) -> Result {
        self.transcript.push(text);
        Ok(())
    }
    pub fn draw(
        &mut self,
        editor: &mut TextArea<'_>,
        footer: &str,
        live: &str,
        plan: bool,
    ) -> Result {
        let transcript = &mut self.transcript;
        let (accent, muted) = (self.accent, self.muted);
        self.terminal
            .draw(|frame| render(frame, transcript, editor, footer, live, plan, accent, muted))?;
        Ok(())
    }
    pub fn navigate(&mut self, event: &Event) -> bool {
        let page = (self.transcript.height / 2).max(1) as isize;
        match event {
            Event::Mouse(m) if m.kind == MouseEventKind::ScrollUp => self.transcript.scroll(-3),
            Event::Mouse(m) if m.kind == MouseEventKind::ScrollDown => self.transcript.scroll(3),
            Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::PageUp => self.transcript.scroll(-page),
                KeyCode::PageDown => self.transcript.scroll(page),
                KeyCode::Home if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.transcript.top = Some(0)
                }
                KeyCode::End if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.transcript.bottom()
                }
                KeyCode::Char('o') if control(k, 'o') => {
                    self.transcript.expanded = !self.transcript.expanded
                }
                KeyCode::F(3) => {
                    self.transcript.find_next();
                }
                _ => return false,
            },
            _ => return false,
        }
        true
    }
    pub fn clear(&mut self) {
        self.transcript.clear();
    }
    pub fn search(&mut self) -> Result {
        let previous = self.transcript.search.clone();
        if let Some(query) =
            self.input("Konuşmada ara · boş bırakınca temizlenir", &previous, false)?
        {
            self.transcript.search = query.to_string();
            let width = self.terminal.size()?.width.saturating_sub(4);
            self.transcript.lines(width, "", self.accent);
            if !self.transcript.search.is_empty() && !self.transcript.find_next() {
                self.print("Aranan metin bulunamadı.")?;
            }
        }
        Ok(())
    }
    pub fn view(&mut self, title: &str, text: &str) -> Result {
        let mut top = 0usize;
        loop {
            self.terminal.draw(|frame| {
                let area = frame.area();
                let block = Block::bordered()
                    .title(format!(" {title} "))
                    .border_style(Style::default().fg(self.accent));
                let inside = block.inner(area);
                frame.render_widget(block, area);
                let regions =
                    Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inside);
                let lines = super::transcript::markdown(text, regions[0].width, self.accent);
                top = top.min(lines.len().saturating_sub(regions[0].height as usize));
                frame.render_widget(
                    Paragraph::new(
                        lines
                            .into_iter()
                            .skip(top)
                            .take(regions[0].height as usize)
                            .collect::<Vec<_>>(),
                    ),
                    regions[0],
                );
                frame.render_widget(
                    Paragraph::new("↑↓ / PgUp PgDn kaydır · Esc geri")
                        .style(Style::default().fg(self.muted)),
                    regions[1],
                );
            })?;
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                    KeyCode::Esc | KeyCode::Enter => return Ok(()),
                    KeyCode::Char('c') if control(&k, 'c') => return Ok(()),
                    KeyCode::Up => top = top.saturating_sub(1),
                    KeyCode::Down => top = top.saturating_add(1),
                    KeyCode::PageUp => top = top.saturating_sub(15),
                    KeyCode::PageDown => top = top.saturating_add(15),
                    KeyCode::Home => top = 0,
                    KeyCode::End => top = usize::MAX,
                    _ => (),
                },
                Event::Mouse(m) if m.kind == MouseEventKind::ScrollUp => {
                    top = top.saturating_sub(3)
                }
                Event::Mouse(m) if m.kind == MouseEventKind::ScrollDown => {
                    top = top.saturating_add(3)
                }
                _ => (),
            }
        }
    }
    pub fn choose(&mut self, title: &str, options: &[String]) -> Result<Option<usize>> {
        let mut query = String::new();
        let mut at = 0usize;
        loop {
            let visible = options
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    query
                        .split_whitespace()
                        .all(|q| s.to_lowercase().contains(&q.to_lowercase()))
                })
                .collect::<Vec<_>>();
            at = at.min(visible.len().saturating_sub(1));
            let accent = self.accent;
            let background = self
                .transcript
                .lines(self.terminal.size()?.width.saturating_sub(4), "", accent)
                .to_vec();
            self.terminal.draw(|frame| {
                let area = modal(frame, &background, title, accent);
                let (prompt, prompt_height) = modal_prompt(title, accent, area.width);
                let areas = Layout::vertical([
                    Constraint::Length(prompt_height),
                    Constraint::Length(1),
                    Constraint::Min(2),
                    Constraint::Length(1),
                ])
                .split(area);
                frame.render_widget(prompt, areas[0]);
                frame.render_widget(
                    Paragraph::new(format!("Ara: {}", display::safe(&query))),
                    areas[1],
                );
                let offset = at.saturating_sub(areas[2].height.max(1) as usize - 1);
                let lines = visible
                    .iter()
                    .skip(offset)
                    .map(|(i, s)| {
                        Line::from(Span::styled(
                            format!(
                                "{} {}",
                                if visible.get(at).is_some_and(|(j, _)| j == i) {
                                    "❯"
                                } else {
                                    " "
                                },
                                display::safe(s)
                            ),
                            Style::default().fg(if visible.get(at).is_some_and(|(j, _)| j == i) {
                                accent
                            } else {
                                Color::Reset
                            }),
                        ))
                    })
                    .collect::<Vec<_>>();
                frame.render_widget(Paragraph::new(lines), areas[2]);
                frame.render_widget(
                    Paragraph::new("↑↓ seç · Enter aç · yazıp ara · Esc geri"),
                    areas[3],
                );
            })?;
            if let Event::Key(k) = event::read()?
                && k.kind != KeyEventKind::Release
            {
                match k.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Enter => return Ok(visible.get(at).map(|(i, _)| *i)),
                    KeyCode::Up => at = at.saturating_sub(1),
                    KeyCode::Down => at = (at + 1).min(visible.len().saturating_sub(1)),
                    KeyCode::Backspace => {
                        query.pop();
                        at = 0;
                    }
                    KeyCode::Char('c') if k.modifiers == KeyModifiers::CONTROL => return Ok(None),
                    KeyCode::Char(c)
                        if !k.modifiers.contains(KeyModifiers::CONTROL)
                            || k.modifiers
                                .contains(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        query.push(c);
                        at = 0;
                    }
                    _ => (),
                }
            }
        }
    }
    pub fn input(
        &mut self,
        title: &str,
        default: &str,
        secret: bool,
    ) -> Result<Option<Zeroizing<String>>> {
        self.input_checked(title, "", default, secret, |_| Ok(()))
    }
    pub fn input_checked(
        &mut self,
        title: &str,
        help: &str,
        default: &str,
        secret: bool,
        validate: impl Fn(&str) -> Result<()>,
    ) -> Result<Option<Zeroizing<String>>> {
        let mut value = Zeroizing::new(default.to_owned());
        let mut cursor = value.len();
        let mut error: Option<String> = None;
        loop {
            let display = if secret {
                "•".repeat(value.chars().count().min(120))
            } else {
                format!(
                    "{}▏{}",
                    display::safe(&value[..cursor]),
                    display::safe(&value[cursor..])
                )
            };
            let accent = self.accent;
            let background = self
                .transcript
                .lines(self.terminal.size()?.width.saturating_sub(4), "", accent)
                .to_vec();
            self.terminal.draw(|frame| {
                let area = modal(frame, &background, title, accent);
                let (prompt, prompt_height) =
                    field_prompt(title, help, error.as_deref(), accent, area.width);
                let areas = Layout::vertical([
                    Constraint::Length(prompt_height),
                    Constraint::Min(2),
                    Constraint::Length(1),
                ])
                .split(area);
                frame.render_widget(prompt, areas[0]);
                frame.render_widget(
                    Paragraph::new(display)
                        .block(Block::default().borders(Borders::BOTTOM))
                        .wrap(Wrap { trim: false }),
                    areas[1],
                );
                frame.render_widget(
                    Paragraph::new("Enter devam · Esc vazgeç · Ctrl+U temizle"),
                    areas[2],
                );
            })?;
            match event::read()? {
                Event::Paste(s) => {
                    if value.len().saturating_add(s.len()) <= 8192 {
                        let s = s.trim_end_matches(['\r', '\n']);
                        value.insert_str(cursor, s);
                        cursor += s.len();
                    }
                }
                Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Enter => match validate(&value) {
                        Ok(()) => return Ok(Some(value)),
                        Err(e) => error = Some(e.to_string()),
                    },
                    KeyCode::Backspace => {
                        if cursor > 0 {
                            let prev = value[..cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            value.replace_range(prev..cursor, "");
                            cursor = prev;
                        }
                    }
                    KeyCode::Delete => {
                        if cursor < value.len() {
                            let end = cursor
                                + value[cursor..]
                                    .chars()
                                    .next()
                                    .map(|c| c.len_utf8())
                                    .unwrap_or(0);
                            value.replace_range(cursor..end, "");
                        }
                    }
                    KeyCode::Left => {
                        cursor = value[..cursor]
                            .char_indices()
                            .last()
                            .map(|(i, _)| i)
                            .unwrap_or(0)
                    }
                    KeyCode::Right => {
                        cursor = (cursor
                            + value[cursor..]
                                .chars()
                                .next()
                                .map(|c| c.len_utf8())
                                .unwrap_or(0))
                        .min(value.len())
                    }
                    KeyCode::Home => cursor = 0,
                    KeyCode::End => cursor = value.len(),
                    KeyCode::Char('u') if k.modifiers == KeyModifiers::CONTROL => {
                        value.clear();
                        cursor = 0;
                    }
                    KeyCode::Char('c') if k.modifiers == KeyModifiers::CONTROL => return Ok(None),
                    KeyCode::Char('v' | 'y') if k.modifiers == KeyModifiers::CONTROL => {
                        if let Ok(s) = crate::frontend::paste::clipboard()
                            && value.len().saturating_add(s.len()) <= 8192
                        {
                            let secret_copy = Zeroizing::new(s);
                            let s = secret_copy.trim_end_matches(['\r', '\n']);
                            value.insert_str(cursor, s);
                            cursor += s.len();
                        }
                    }
                    KeyCode::Char(c)
                        if (!k.modifiers.contains(KeyModifiers::CONTROL)
                            || k.modifiers
                                .contains(KeyModifiers::CONTROL | KeyModifiers::ALT))
                            && value.len() < 8192 =>
                    {
                        value.insert(cursor, c);
                        cursor += c.len_utf8();
                    }
                    _ => (),
                },
                _ => (),
            }
        }
    }
    pub fn confirm(&mut self, title: &str) -> Result<bool> {
        Ok(self.choose(title, &["Evet".into(), "Vazgeç".into()])? == Some(0))
    }
    pub fn key(&self) -> Result<Option<Event>> {
        if event::poll(Duration::from_millis(20))? {
            Ok(Some(event::read()?))
        } else {
            Ok(None)
        }
    }
}
fn modal(
    frame: &mut ratatui::Frame<'_>,
    background: &[Line<'static>],
    title: &str,
    accent: Color,
) -> Rect {
    let area = frame.area();
    frame.render_widget(
        Paragraph::new(background.to_vec()).style(Style::default().fg(Color::DarkGray)),
        area,
    );
    let width = area.width.saturating_sub(4).min(92).max(area.width.min(12));
    let height = area
        .height
        .saturating_sub(4)
        .min(24)
        .max(area.height.min(6));
    let dialog = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, dialog);
    let block = Block::bordered()
        .title(format!(" {} ", display::safe(title)))
        .border_style(Style::default().fg(accent));
    let inside = block.inner(dialog);
    frame.render_widget(block, dialog);
    inside
}
#[allow(clippy::too_many_arguments)]
fn render(
    frame: &mut ratatui::Frame<'_>,
    transcript: &mut super::transcript::Transcript,
    editor: &mut TextArea<'_>,
    footer: &str,
    live: &str,
    plan: bool,
    accent: Color,
    muted: Color,
) {
    let area = frame.area();
    let margin = if area.width >= 45 { 2 } else { 1 };
    let inside = Rect::new(
        area.x + margin,
        area.y,
        area.width.saturating_sub(margin * 2),
        area.height,
    );
    let editor_height = (editor.lines().len() as u16 + 2).clamp(3, (area.height / 3).max(3));
    let regions = Layout::vertical([
        Constraint::Length(if area.height < 12 { 1 } else { 2 }),
        Constraint::Min(1),
        Constraint::Length(if area.height < 12 { 0 } else { 1 }),
        Constraint::Length(editor_height),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inside);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "✦ Melissa",
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", env!("CARGO_PKG_VERSION")),
                Style::default().fg(muted),
            ),
        ])),
        regions[0],
    );
    transcript.height = regions[1].height as usize;
    let lines = transcript.lines(regions[1].width, live, accent).to_vec();
    let end = lines.len().saturating_sub(transcript.height);
    let start = transcript.top.unwrap_or(end).min(end);
    if lines.is_empty() {
        let welcome = vec![
            Line::from(""),
            Line::from(Span::styled(
                "Ne yapmak istersin?",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Bir soru sor, bir fikri planla veya kod üzerinde çalış."),
            Line::from(""),
            Line::from(Span::styled(
                "/settings  model bağlantısı     /resume  konuşmalar",
                Style::default().fg(muted),
            )),
        ];
        frame.render_widget(
            Paragraph::new(welcome).wrap(Wrap { trim: false }),
            regions[1],
        );
    } else {
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(start)
                    .take(transcript.height)
                    .collect::<Vec<_>>(),
            ),
            regions[1],
        );
    }
    let hint = if transcript.top.is_some() {
        "↑ Geçmiştesin · Ctrl+End son mesaja dön · Ctrl+F ara"
    } else if plan {
        "Plan açık · dosyalar değişmez · Shift+Tab ile normal sohbete dön"
    } else if !live.is_empty() {
        "● Yanıt geliyor · yazmaya devam edebilirsin · Esc durdur"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(muted)),
        regions[2],
    );
    editor.set_block(
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(accent))
            .title(if plan { " ❯ Plan " } else { " ❯ " }),
    );
    frame.render_widget(&*editor, regions[3]);
    frame.render_widget(
        Paragraph::new(display::safe(footer)).style(Style::default().fg(muted)),
        regions[4],
    );
    frame.render_widget(Paragraph::new(if area.width >= 90 { "Enter gönder · Alt+Enter yeni satır · Shift+Tab plan · / komutlar · Ctrl+O araçlar · Ctrl+F ara" } else { "Enter gönder · Shift+Tab plan · / komutlar · Esc durdur" }).style(Style::default().fg(muted)), regions[5]);
}
pub(crate) fn editor() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text("Bir şey sor veya yapmak istediğini yaz…");
    input.set_cursor_line_style(Style::default());
    input
}
pub(crate) fn control(key: &KeyEvent, c: char) -> bool {
    key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char(c)
}
pub(crate) fn status_line(
    model: &str,
    folder: &str,
    activity: &str,
    cost: &str,
    queued: usize,
    width: u16,
) -> String {
    use unicode_width::UnicodeWidthStr;
    fn short(text: &str, width: usize) -> String {
        use unicode_segmentation::UnicodeSegmentation;
        use unicode_width::UnicodeWidthStr;
        if text.width() <= width {
            return text.into();
        }
        if width == 0 {
            return String::new();
        }
        let mut out = String::new();
        for g in text.graphemes(true) {
            if out.width() + g.width() + 1 > width {
                break;
            }
            out.push_str(g);
        }
        out.push('…');
        out
    }
    let cost = if cost == "maliyet bilinmiyor" {
        "tutar ?"
    } else {
        cost
    };
    let tail = format!(
        "{activity}{} · {cost}",
        if queued == 0 {
            String::new()
        } else {
            format!(" · {queued} sırada")
        }
    );
    let width = width as usize;
    let available = width.saturating_sub(tail.width() + 6);
    if available < 12 {
        return short(&tail, width);
    }
    let model_width = (available * 3 / 5).min(model.width());
    format!(
        "{} · {} · {tail}",
        short(model, model_width),
        short(folder, available.saturating_sub(model_width))
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_modal_prompt_shows_the_end_of_the_confirmation() {
        let title = "Seçilen API mesajlarını ve gerekli görünür proje içeriğini alabilir; ücret doğabilir. Devam etmek istiyor musun?";
        for width in [32, 74, 90] {
            let (prompt, height) = modal_prompt(title, Color::Cyan, width);
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| frame.render_widget(prompt, frame.area()))
                .unwrap();
            let content = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(content.contains("musun?"));
            assert!(height > 1);
        }
    }
    #[test]
    fn status_preserves_activity_on_narrow_terminals() {
        use unicode_width::UnicodeWidthStr;
        for width in [30, 40, 76, 116] {
            let text = status_line(
                "a-very-long-custom-model-name",
                "my-website-project",
                "PLAN",
                "maliyet bilinmiyor",
                0,
                width,
            );
            assert!(text.width() <= width as usize);
            assert!(text.contains("PLAN"));
        }
    }
    #[test]
    fn full_screen_layout_keeps_prompt_on_small_and_large_terminals() {
        for (width, height) in [(120, 40), (80, 24), (40, 12), (20, 8)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            let mut transcript = super::super::transcript::Transcript::default();
            let mut input = editor();
            input.insert_str("Türkçe soru");
            terminal
                .draw(|f| {
                    render(
                        f,
                        &mut transcript,
                        &mut input,
                        "model · sohbet",
                        "",
                        false,
                        Color::Cyan,
                        Color::DarkGray,
                    )
                })
                .unwrap();
            let content = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(content.contains("Melissa"));
            assert!(content.contains("Türkçe soru"));
        }
    }
}
