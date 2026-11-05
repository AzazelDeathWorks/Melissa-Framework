use super::{ACCENT, COMMANDS, MUTED, Ui};
use crate::{Result, display};
use crossterm::{
    cursor::MoveToColumn,
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute, queue,
    terminal::{self, Clear, ClearType},
};
use std::io::{BufRead, Write};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) struct RawMode;
impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), DisableBracketedPaste);
        let _ = terminal::disable_raw_mode();
    }
}

/// Nonblocking editor used while the execution future stays polled. Ctrl+C is
/// a task cancellation request here, never permission to drop the live future.
pub(super) struct RunningInput {
    _raw: Option<RawMode>,
    text: String,
    cursor: usize,
}
impl RunningInput {
    pub fn new(output: &mut impl Write) -> Result<Self> {
        terminal::enable_raw_mode()?;
        let raw = RawMode;
        execute!(output, EnableBracketedPaste)?;
        Ok(Self {
            _raw: Some(raw),
            text: String::new(),
            cursor: 0,
        })
    }
    pub fn poll(&mut self) -> Result<Option<String>> {
        if !event::poll(std::time::Duration::ZERO)? {
            return Ok(None);
        }
        self.accept(event::read()?)
    }
    fn accept(&mut self, event: Event) -> Result<Option<String>> {
        match event {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                let alt_gr = k
                    .modifiers
                    .contains(KeyModifiers::CONTROL | KeyModifiers::ALT);
                if k.modifiers.contains(KeyModifiers::CONTROL) && !alt_gr {
                    if k.code == KeyCode::Char('c') {
                        self.text.clear();
                        self.cursor = 0;
                        return Ok(Some("/cancel".into()));
                    }
                    return Ok(None);
                }
                match k.code {
                    KeyCode::Enter => {
                        self.cursor = 0;
                        return Ok(Some(std::mem::take(&mut self.text)));
                    }
                    KeyCode::Char(c) if alt_gr || !k.modifiers.contains(KeyModifiers::ALT) => {
                        self.text.insert(self.cursor, c);
                        self.cursor = next_boundary(&self.text, self.cursor + c.len_utf8());
                    }
                    KeyCode::Backspace if self.cursor > 0 => {
                        let p = previous_boundary(&self.text, self.cursor);
                        self.text.replace_range(p..self.cursor, "");
                        self.cursor = p;
                    }
                    KeyCode::Delete if self.cursor < self.text.len() => {
                        let n = next_boundary(&self.text, self.cursor + 1);
                        self.text.replace_range(self.cursor..n, "");
                    }
                    KeyCode::Left => self.cursor = previous_boundary(&self.text, self.cursor),
                    KeyCode::Right if self.cursor < self.text.len() => {
                        self.cursor = next_boundary(&self.text, self.cursor + 1)
                    }
                    KeyCode::Home => self.cursor = 0,
                    KeyCode::End => self.cursor = self.text.len(),
                    KeyCode::Esc => {
                        self.text.clear();
                        self.cursor = 0;
                    }
                    _ => {}
                }
            }
            Event::Paste(s) => {
                let s = s.replace(['\r', '\n', '\t'], " ");
                self.text.insert_str(self.cursor, &s);
                self.cursor = next_boundary(&self.text, self.cursor + s.len());
            }
            _ => {}
        }
        Ok(None)
    }
    pub fn draw(&self, ui: &Ui, output: &mut impl Write) -> Result {
        ui.redraw(&self.text, self.cursor, output)
    }
}

impl Ui {
    pub fn read_line(
        &mut self,
        input: &mut impl BufRead,
        output: &mut impl Write,
        context: &str,
    ) -> Result<Option<String>> {
        if !self.interactive {
            write!(output, "melissa> ")?;
            output.flush()?;
            let mut line = String::new();
            return Ok((input.read_line(&mut line)? != 0).then_some(line));
        }
        writeln!(output)?;
        writeln!(output, "  {}", self.paint(context, MUTED))?;
        writeln!(
            output,
            "  {}",
            self.paint(&"─".repeat(self.width().saturating_sub(4)), MUTED)
        )?;
        terminal::enable_raw_mode()?;
        let raw = RawMode;
        execute!(output, EnableBracketedPaste)?;
        let mut text = String::new();
        let mut cursor = 0;
        let mut index = self.history.len();
        let mut saved = String::new();
        let result = loop {
            self.redraw(&text, cursor, output)?;
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    // Windows reports AltGr text, including Turkish-layout i
                    // and programming symbols, as Control+Alt key events.
                    let alt_gr = key
                        .modifiers
                        .contains(KeyModifiers::CONTROL | KeyModifiers::ALT);
                    if key.modifiers.contains(KeyModifiers::CONTROL) && !alt_gr {
                        match key.code {
                            KeyCode::Char('c') if !text.is_empty() => {
                                text.clear();
                                cursor = 0;
                            }
                            KeyCode::Char('c' | 'd') if text.is_empty() => break None,
                            KeyCode::Char('a') => cursor = 0,
                            KeyCode::Char('e') => cursor = text.len(),
                            _ => {}
                        }
                        continue;
                    }
                    match key.code {
                        KeyCode::Enter => break Some(text),
                        KeyCode::Char(c)
                            if alt_gr || !key.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            text.insert(cursor, c);
                            cursor = next_boundary(&text, cursor + c.len_utf8());
                        }
                        KeyCode::Backspace if cursor > 0 => {
                            let previous = previous_boundary(&text, cursor);
                            text.replace_range(previous..cursor, "");
                            cursor = previous;
                        }
                        KeyCode::Delete if cursor < text.len() => {
                            let next = next_boundary(&text, cursor + 1);
                            text.replace_range(cursor..next, "");
                        }
                        KeyCode::Left => cursor = previous_boundary(&text, cursor),
                        KeyCode::Right if cursor < text.len() => {
                            cursor = next_boundary(&text, cursor + 1)
                        }
                        KeyCode::Home => cursor = 0,
                        KeyCode::End => cursor = text.len(),
                        KeyCode::Esc => {
                            text.clear();
                            cursor = 0;
                        }
                        KeyCode::Up if index > 0 => {
                            if index == self.history.len() {
                                saved = text.clone();
                            }
                            index -= 1;
                            text.clone_from(&self.history[index]);
                            cursor = text.len();
                        }
                        KeyCode::Down if index < self.history.len() => {
                            index += 1;
                            text = self.history.get(index).unwrap_or(&saved).clone();
                            cursor = text.len();
                        }
                        KeyCode::Tab
                            if text.starts_with('/') && !text.contains(char::is_whitespace) =>
                        {
                            let matches: Vec<_> = COMMANDS
                                .iter()
                                .filter(|(name, _)| name.starts_with(&text))
                                .collect();
                            if matches.len() == 1 {
                                text = format!("{} ", matches[0].0);
                                cursor = text.len();
                            }
                        }
                        _ => {}
                    }
                }
                Event::Paste(paste) => {
                    let paste = paste.replace(['\r', '\n', '\t'], " ");
                    text.insert_str(cursor, &paste);
                    cursor = next_boundary(&text, cursor + paste.len());
                }
                Event::Resize(_, _) => {}
                _ => continue,
            }
        };
        let shown = result.as_deref().unwrap_or("");
        self.redraw(shown, shown.len(), output)?;
        drop(raw);
        writeln!(output)?;
        if let Some(line) = &result
            && !line.trim().is_empty()
            && self.history.last() != Some(line)
        {
            self.history.push(line.clone());
        }
        Ok(result)
    }

    fn redraw(&self, text: &str, cursor: usize, output: &mut impl Write) -> Result {
        let safe = display::safe(text);
        let cursor = display::safe(&text[..cursor]).len();
        let (visible, column) = viewport(&safe, cursor, self.width().saturating_sub(8));
        queue!(output, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        write!(output, "  {} ", self.paint("❯", ACCENT))?;
        if text.is_empty() {
            let (placeholder, _) = viewport(
                "Görevini yaz veya /help…",
                0,
                self.width().saturating_sub(8).max(2),
            );
            write!(output, "{}", self.paint(&placeholder, MUTED))?;
        } else {
            write!(output, "{visible}")?;
        }
        queue!(output, MoveToColumn((4 + column) as u16))?;
        output.flush()?;
        Ok(())
    }
}

fn previous_boundary(text: &str, cursor: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|&index| index < cursor)
        .last()
        .unwrap_or(0)
}

fn next_boundary(text: &str, cursor: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .find(|&index| index >= cursor)
        .unwrap_or(text.len())
}

fn viewport(text: &str, cursor: usize, width: usize) -> (String, usize) {
    let mut start = cursor;
    let mut occupied = 0;
    for (index, grapheme) in text[..cursor].grapheme_indices(true).rev() {
        if occupied + grapheme.width() > width.saturating_sub(2) {
            break;
        }
        start = index;
        occupied += grapheme.width();
    }
    let prefix = if start == 0 { "" } else { "‹" };
    let mut visible = prefix.to_owned();
    for grapheme in text[start..].graphemes(true) {
        if visible.width() + grapheme.width() > width {
            break;
        }
        visible.push_str(grapheme);
    }
    let column = prefix.width() + text[start..cursor].width();
    (visible, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_editor_handles_turkish_paste_unicode_cursor_and_control_without_exiting() {
        use crossterm::event::KeyEvent;
        let mut editor = RunningInput {
            _raw: None,
            text: String::new(),
            cursor: 0,
        };
        editor
            .accept(Event::Paste("Türkçe e\u{301} 🦀\n".into()))
            .unwrap();
        editor
            .accept(Event::Key(KeyEvent::new(
                KeyCode::Backspace,
                KeyModifiers::NONE,
            )))
            .unwrap();
        editor
            .accept(Event::Key(KeyEvent::new(
                KeyCode::Char('i'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            )))
            .unwrap();
        let line = editor
            .accept(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .unwrap()
            .unwrap();
        assert_eq!(line, "Türkçe e\u{301} 🦀i");
        assert_eq!(
            editor
                .accept(Event::Key(KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL
                )))
                .unwrap(),
            Some("/cancel".into())
        );
        assert_eq!(
            editor
                .accept(Event::Key(KeyEvent::new(
                    KeyCode::Char('x'),
                    KeyModifiers::NONE
                )))
                .unwrap(),
            None
        );
        assert_eq!(
            editor
                .accept(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                )))
                .unwrap(),
            Some("x".into())
        );
    }

    #[test]
    fn cursor_and_viewport_keep_grapheme_boundaries_and_cursor_visible() {
        let value = "hello e\u{301} 東京 🦀";
        let end = value.len();
        let previous = previous_boundary(value, end);
        assert_eq!(&value[previous..], "🦀");
        assert_eq!(next_boundary(value, previous + 1), end);
        for width in [12, 24, 80] {
            let (visible, cursor) = viewport(value, end, width);
            assert!(visible.width() <= width);
            assert!(cursor < width);
        }
        let long = "x".repeat(16384);
        let (visible, cursor) = viewport(&long, long.len(), 24);
        assert!(visible.starts_with('‹'));
        assert!(visible.width() <= 24);
        assert!(cursor < 24);
    }
}
