use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, KeyEventKind};
use tokio::sync::mpsc;

/// Events that flow from the crossterm polling thread to the main event loop.
#[derive(Debug)]
pub enum Event {
    /// A key press (only Press kind, not Release/Repeat).
    Key(KeyEvent),
    /// Bracketed paste content (multi-line text).
    Paste(String),
    /// Terminal resized.
    #[allow(dead_code)]
    Resize(u16, u16),
    /// Render tick.
    Tick,
}

/// Spawns a dedicated std::thread that polls crossterm events.
pub struct EventHandler {
    rx: mpsc::UnboundedReceiver<Event>,
    _thread: thread::JoinHandle<()>,
}

impl EventHandler {
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let thread = thread::spawn(move || {
            loop {
                if event::poll(tick_rate).unwrap_or(false) {
                    match event::read() {
                        Ok(CrosstermEvent::Key(key))
                            if key.kind == KeyEventKind::Press
                                && tx.send(Event::Key(key)).is_err() =>
                        {
                            break;
                        }
                        Ok(CrosstermEvent::Paste(data)) => {
                            if tx.send(Event::Paste(data)).is_err() {
                                break;
                            }
                        }
                        Ok(CrosstermEvent::Resize(w, h))
                            if tx.send(Event::Resize(w, h)).is_err() =>
                        {
                            break;
                        }
                        _ => {}
                    }
                } else {
                    if tx.send(Event::Tick).is_err() {
                        break;
                    }
                }
            }
        });

        Self {
            rx,
            _thread: thread,
        }
    }

    pub async fn recv(&mut self) -> Option<Event> {
        self.rx.recv().await
    }

    /// Non-blocking receive — returns None if queue is empty.
    pub fn try_recv(&mut self) -> Option<Event> {
        self.rx.try_recv().ok()
    }
}
