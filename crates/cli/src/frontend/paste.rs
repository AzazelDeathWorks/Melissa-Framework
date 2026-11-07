//! Windows Console key records need explicit bracketed-paste reassembly.
//! Clipboard access is exclusively an operator Ctrl+V/Ctrl+Y gesture.
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
#[derive(Default)]
pub(crate) struct Paste {
    prefix: String,
    text: Option<String>,
}
impl Paste {
    pub(crate) fn flush_escape(&mut self) -> Option<Event> {
        if self.text.is_none() && self.prefix == "\x1b" {
            self.prefix.clear();
            return Some(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )));
        }
        None
    }
    pub fn push(&mut self, event: Event) -> Option<Event> {
        if matches!(&event,Event::Key(k) if k.modifiers==KeyModifiers::CONTROL && matches!(k.code,KeyCode::Char('c'|'d')))
        {
            self.prefix.clear();
            self.text = None;
            return Some(event);
        }
        let ch = match &event {
            Event::Key(k) if k.kind == KeyEventKind::Release => return None,
            Event::Key(k) => match k.code {
                KeyCode::Char(c) => Some(c),
                KeyCode::Esc => Some('\x1b'),
                KeyCode::Enter => Some('\n'),
                KeyCode::Tab => Some('\t'),
                _ => None,
            },
            _ => None,
        };
        let Some(ch) = ch else { return Some(event) };
        if let Some(text) = &mut self.text {
            text.push(ch);
            if text.ends_with("\x1b[201~") {
                text.truncate(text.len() - 6);
                return Some(Event::Paste(self.text.take().unwrap()));
            }
            return None;
        }
        if self.prefix.is_empty() && ch != '\x1b' {
            return Some(event);
        }
        self.prefix.push(ch);
        if self.prefix == "\x1b[200~" {
            self.prefix.clear();
            self.text = Some(String::new());
            return None;
        }
        if "\x1b[200~".starts_with(&self.prefix) {
            return None;
        }
        self.prefix.clear();
        Some(event)
    }
}
pub(crate) fn clipboard() -> crate::Result<String> {
    use windows_sys::Win32::System::{
        DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
        Memory::{GlobalLock, GlobalSize, GlobalUnlock},
    };
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err(crate::Error::Unavailable("Pano şu anda okunamıyor."));
        }
        struct Close;
        impl Drop for Close {
            fn drop(&mut self) {
                unsafe {
                    CloseClipboard();
                }
            }
        }
        let _close = Close;
        let data = GetClipboardData(13);
        if data.is_null() {
            return Err(crate::Error::Unavailable("Panoda Unicode metin yok."));
        }
        let size = GlobalSize(data);
        if size == 0 || size > 32 * 1024 * 1024 {
            return Err(crate::Error::Unavailable(
                "Pano metni geçersiz veya tek aktarım için çok büyük.",
            ));
        }
        let ptr = GlobalLock(data).cast::<u16>();
        if ptr.is_null() {
            return Err(crate::Error::Unavailable("Pano metni açılamadı."));
        }
        let chars = std::slice::from_raw_parts(ptr, size / 2);
        let n = chars.iter().position(|c| *c == 0).unwrap_or(chars.len());
        let value = String::from_utf16_lossy(&chars[..n]);
        GlobalUnlock(data);
        Ok(value.replace("\r\n", "\n").replace('\r', "\n"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    #[test]
    fn cancellation_recovers_from_an_interrupted_terminal_paste() {
        let mut paste = Paste::default();
        for c in "\x1b[200~partial".chars() {
            let code = if c == '\x1b' {
                KeyCode::Esc
            } else {
                KeyCode::Char(c)
            };
            paste.push(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        }
        let cancel = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(paste.push(cancel.clone()), Some(cancel));
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(paste.push(enter.clone()), Some(enter));
    }
    #[test]
    fn console_paste_markers_hold_newlines_and_do_not_submit_partial_code() {
        let mut paste = Paste::default();
        let mut received = vec![];
        for c in "\x1b[200~Türkçe\n\t🦀\x1b[201~".chars() {
            let key = match c {
                '\x1b' => KeyCode::Esc,
                '\n' => KeyCode::Enter,
                '\t' => KeyCode::Tab,
                c => KeyCode::Char(c),
            };
            if let Some(event) = paste.push(Event::Key(KeyEvent::new(key, KeyModifiers::NONE))) {
                received.push(event);
            }
        }
        assert_eq!(received, vec![Event::Paste("Türkçe\n\t🦀".into())]);
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(paste.push(enter.clone()), Some(enter));
    }
}
