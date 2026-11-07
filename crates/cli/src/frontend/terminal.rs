//! Inline Ratatui composer: ordinary terminal scrollback is retained.
use super::{Action, Request, Response, pipe::Client};
use crate::{Result, display};
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use ratatui_textarea::TextArea;
use std::{
    io::{self, IsTerminal},
    path::Path,
    time::{Duration, Instant},
};

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste);
    }
}
pub async fn run(home: &Path, workspace: &Path) -> Result {
    let client = Client::connect_or_start(home, workspace).await?;
    let mut state = client.request(request(Action::Snapshot, 0)?).await?;
    let colors =
        io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
    let accent = if colors {
        Color::Rgb(167, 139, 250)
    } else {
        Color::Reset
    };
    enable_raw_mode()?;
    let _restore = Restore;
    execute!(io::stdout(), EnableBracketedPaste)?;
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(9),
        },
    )?;
    let title = format!(
        "◆ MELISSA  {}   ·   {}\nKodunu anla. Planını incele. Birlikte geliştir.\n/help komutlar · /exit çıkış",
        env!("CARGO_PKG_VERSION"),
        workspace.display()
    );
    insert(&mut terminal, &title, accent)?;
    let mut input = editor();
    let mut paste = super::paste::Paste::default();
    let mut history: Vec<String> = vec![];
    let mut history_at = 0usize;
    let mut pending: Option<tokio::task::JoinHandle<Result<Response>>> = None;
    let mut last_poll = Instant::now();
    let mut queued: Option<(String, Request)> = None;
    loop {
        if pending.as_ref().is_some_and(|p| p.is_finished()) {
            match pending.take().unwrap().await {
                Ok(Ok(reply)) => accept(&mut terminal, &mut state, reply, accent)?,
                Ok(Err(e)) => insert(
                    &mut terminal,
                    &format!("Bağlantı: {e}. Görev kaydı korunuyor; /status ile yeniden oku."),
                    Color::Yellow,
                )?,
                Err(_) => insert(
                    &mut terminal,
                    "Arayüz isteği tamamlanamadı. Görev durumunu yeniden oku.",
                    Color::Yellow,
                )?,
            }
        }
        if pending.is_none()
            && let Some((text, req)) = queued.take()
        {
            if text.trim() == "/exit" && state.state["busy"] != true {
                break;
            }
            let c = client.clone();
            pending = Some(tokio::spawn(async move { c.request(req).await }));
        }
        if pending.is_none() && last_poll.elapsed() > Duration::from_millis(300) {
            let c = client.clone();
            let req = request(Action::Snapshot, state.cursor)?;
            pending = Some(tokio::spawn(async move { c.request(req).await }));
            last_poll = Instant::now();
        }
        let text = input.lines().join("\n");
        let suggestions = crate::session_ui::COMMANDS
            .iter()
            .filter(|(name, _)| {
                text.starts_with('/')
                    && !text.contains(char::is_whitespace)
                    && name.starts_with(&text)
            })
            .take(3)
            .map(|(n, d)| format!("{n}  {d}"))
            .collect::<Vec<_>>()
            .join("   ·   ");
        terminal.draw(|frame|{
            let areas=Layout::vertical([Constraint::Length(1),Constraint::Min(3),Constraint::Length(1),Constraint::Length(1)]).split(frame.area());
            let task=state.state["task_id"].as_str().unwrap_or("Yeni görev");
            let status=if state.state["busy"]==true{"● Çalışıyor"}else if state.state["approval"].is_object(){"◈ Onay bekliyor"}else{"○ Hazır"};
            frame.render_widget(Paragraph::new(Line::from(vec![Span::styled(format!(" {status} "),Style::default().fg(accent)),Span::raw(format!(" · {task}"))])),areas[0]);
            input.set_block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(accent)).title(" Görevini yaz "));
            frame.render_widget(&input,areas[1]);
            frame.render_widget(Paragraph::new(if suggestions.is_empty(){state.state["context"].as_str().unwrap_or("")}else{&suggestions}).style(Style::default().fg(if colors{Color::DarkGray}else{Color::Reset})),areas[2]);
            let live=state.state["visible_text"]["text"].as_str().and_then(|s|s.lines().last()).map(display::safe);
            frame.render_widget(Paragraph::new(live.as_deref().unwrap_or("Enter gönder · Alt/Shift+Enter veya Ctrl+J yeni satır · Ctrl+Y yapıştır · Ctrl+Z geri al · Ctrl+C iptal")),areas[3]);
        })?;
        let drain_started = Instant::now();
        while drain_started.elapsed() < Duration::from_millis(8) && event::poll(Duration::ZERO)? {
            let Some(event) = paste.push(event::read()?) else {
                continue;
            };
            match event {
                Event::Paste(s) => {
                    input.insert_str(s.replace("\r\n", "\n").replace('\r', "\n"));
                }
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    let text = input.lines().join("\n");
                    let altgr = k
                        .modifiers
                        .contains(KeyModifiers::CONTROL | KeyModifiers::ALT);
                    if k.modifiers == KeyModifiers::CONTROL
                        && matches!(k.code, KeyCode::Char('v' | 'y'))
                    {
                        match super::paste::clipboard() {
                            Ok(text) => {
                                input.insert_str(text);
                            }
                            Err(e) => insert(&mut terminal, &e.to_string(), Color::Yellow)?,
                        }
                        continue;
                    }
                    let submit = matches!(k.code, KeyCode::Enter) && k.modifiers.is_empty();
                    if k.code == KeyCode::Enter && !submit
                        || k.code == KeyCode::Char('j') && k.modifiers == KeyModifiers::CONTROL
                    {
                        input.insert_newline();
                    } else if k.code == KeyCode::Char('c') && k.modifiers == KeyModifiers::CONTROL {
                        if state.state["busy"] == true && pending.is_none() {
                            pending = Some(send(&client, &state, "/cancel")?);
                        } else if text.is_empty() && state.state["busy"] != true {
                            break;
                        } else {
                            input = editor();
                        }
                    } else if k.code == KeyCode::Char('d')
                        && k.modifiers == KeyModifiers::CONTROL
                        && text.is_empty()
                        && state.state["busy"] != true
                    {
                        break;
                    } else if submit && pending.is_some() && !text.trim().is_empty() {
                        if queued.is_none() {
                            let command = if text.trim() == "/exit" {
                                "/pause"
                            } else {
                                &text
                            };
                            queued = Some((text.clone(), input_request(&state, command)?));
                            insert(&mut terminal, &format!("❯ {text}"), accent)?;
                            history.push(text.clone());
                            history_at = history.len();
                            input = editor();
                        }
                    } else if submit && pending.is_none() && !text.trim().is_empty() {
                        if text.trim() == "/exit" {
                            if state.state["busy"] == true {
                                pending = Some(send(&client, &state, "/pause")?);
                                insert(
                                    &mut terminal,
                                    "Duraklatma istendi; güvenli duruş sonrası /exit kullan.",
                                    Color::Yellow,
                                )?;
                            } else {
                                break;
                            }
                        } else {
                            insert(&mut terminal, &format!("❯ {text}"), accent)?;
                            pending = Some(send(&client, &state, &text)?);
                            history.push(text);
                            history_at = history.len();
                            input = editor();
                        }
                    } else if k.code == KeyCode::Tab && !suggestions.is_empty() {
                        if let Some((name, _)) = crate::session_ui::COMMANDS
                            .iter()
                            .find(|(n, _)| n.starts_with(&text))
                        {
                            input = editor();
                            input.insert_str(format!("{name} "));
                        }
                    } else if k.code == KeyCode::Up && input.lines().len() == 1 && history_at > 0 {
                        history_at -= 1;
                        input = editor();
                        input.insert_str(&history[history_at]);
                    } else if k.code == KeyCode::Down && history_at < history.len() {
                        history_at += 1;
                        input = editor();
                        if let Some(h) = history.get(history_at) {
                            input.insert_str(h);
                        }
                    } else if altgr {
                        if let KeyCode::Char(c) = k.code {
                            input.insert_str(c.to_string());
                        }
                    } else {
                        input.input(k);
                    }
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    terminal.clear()?;
    terminal.show_cursor()?;
    Ok(())
}
fn editor() -> TextArea<'static> {
    let mut area = TextArea::default();
    area.set_placeholder_text("Bir hata düzelt, kodu incele veya yeni bir özellik geliştir…");
    area.set_cursor_line_style(Style::default());
    area
}
fn request(action: Action, after: u64) -> Result<Request> {
    Ok(Request {
        version: 1,
        request_id: crate::app::nonce()?,
        after,
        action,
    })
}
fn send(
    client: &Client,
    state: &Response,
    text: &str,
) -> Result<tokio::task::JoinHandle<Result<Response>>> {
    let req = input_request(state, text)?;
    let client = client.clone();
    Ok(tokio::spawn(async move { client.request(req).await }))
}
fn input_request(state: &Response, text: &str) -> Result<Request> {
    request(
        Action::Input {
            text: text.into(),
            expected_revision: state.state["revision"].as_u64().unwrap_or(0),
            approval_digest: state.state["approval"]["digest"]
                .as_str()
                .map(str::to_owned),
        },
        state.cursor,
    )
}

fn accept(
    t: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut Response,
    reply: Response,
    accent: Color,
) -> Result {
    if reply.replay_reset {
        insert(
            t,
            "Yeniden bağlanıldı. Eski olaylar görev günlüklerinde korunuyor.",
            Color::Yellow,
        )?;
    }
    for e in &reply.events {
        let text = super::presentation::summary(&e.kind, &e.data);
        if e.kind != "progress" {
            insert(
                t,
                &text,
                if e.kind == "error" {
                    Color::Yellow
                } else {
                    Color::Reset
                },
            )?;
        }
    }
    if let Some(e) = &reply.error {
        insert(
            t,
            e["message"].as_str().unwrap_or("İşlem tamamlanamadı."),
            Color::Yellow,
        )?;
    }
    if !reply.value.is_null() {
        insert(t, &display::human(&reply.value)?, accent)?;
    }
    *state = reply;
    Ok(())
}
fn insert(t: &mut Terminal<CrosstermBackend<io::Stdout>>, text: &str, color: Color) -> Result {
    let text = text
        .lines()
        .map(display::safe)
        .collect::<Vec<_>>()
        .join("\n");
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let width = t.size()?.width.max(1) as usize;
    let color = if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        Color::Reset
    } else {
        color
    };
    let mut lines = vec![];
    for source in text.lines() {
        let mut line = String::new();
        for grapheme in source.graphemes(true) {
            if !line.is_empty() && line.width() + grapheme.width() > width {
                lines.push(std::mem::take(&mut line));
            }
            line.push_str(grapheme);
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    for chunk in lines.chunks(200) {
        let text = chunk.join("\n");
        t.insert_before(chunk.len() as u16, |buffer| {
            use ratatui::widgets::Widget;
            Paragraph::new(text)
                .style(Style::default().fg(color))
                .render(buffer.area, buffer);
        })?;
    }
    Ok(())
}
