use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, KeyEventKind};

/// Events that flow from the crossterm polling thread to the main event loop.
#[derive(Debug)]
pub enum Event {
    /// A key press (only Press kind, not Release/Repeat).
    Key(KeyEvent),
    /// Terminal resized.
    Resize(u16, u16),
    /// Render tick.
    Tick,
}

/// Spawns a dedicated std::thread that polls crossterm events.
///
/// crossterm's `event::read()` is blocking — it cannot run on the tokio runtime
/// without starving other tasks. A dedicated OS thread solves this cleanly.
pub struct EventHandler {
    rx: mpsc::Receiver<Event>,
    _thread: thread::JoinHandle<()>,
}

impl EventHandler {
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::channel();

        let thread = thread::spawn(move || {
            loop {
                // Poll with the tick rate as timeout.
                if event::poll(tick_rate).unwrap_or(false) {
                    match event::read() {
                        Ok(CrosstermEvent::Key(key)) => {
                            // Only forward actual key presses, not release/repeat.
                            if key.kind == KeyEventKind::Press {
                                if tx.send(Event::Key(key)).is_err() {
                                    break; // receiver dropped, main loop exited
                                }
                            }
                        }
                        Ok(CrosstermEvent::Resize(w, h)) => {
                            if tx.send(Event::Resize(w, h)).is_err() {
                                break;
                            }
                        }
                        _ => {}
                    }
                } else {
                    // Timeout elapsed — send a tick for rendering.
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

    /// Non-blocking receive. Returns `None` if no event is available.
    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// Blocking receive with timeout.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        self.rx.recv_timeout(timeout).ok()
    }
}
