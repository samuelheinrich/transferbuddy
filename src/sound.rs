//! Tiny "PC speaker" feedback for the TUI.
//!
//! There is no real PC speaker on a Mac, so the closest equivalents are used:
//! short system sounds via `afplay` (spawned in a detached thread so the UI
//! never blocks), falling back to the terminal bell when `afplay` or the sound
//! files are unavailable. Every call is a no-op when sound is switched off.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Something was switched on / started.
    On,
    /// Something was switched off / stopped.
    Off,
    /// A value was accepted (port, bind address, password, ...).
    Confirm,
    /// A value was rejected.
    Error,
    /// Start-up jingle for the intro screen.
    Boot,
}

impl Tone {
    /// macOS system sound that comes closest to the intended blip.
    fn sound_file(self) -> &'static str {
        match self {
            Tone::On => "/System/Library/Sounds/Tink.aiff",
            Tone::Off => "/System/Library/Sounds/Pop.aiff",
            Tone::Confirm => "/System/Library/Sounds/Bottle.aiff",
            Tone::Error => "/System/Library/Sounds/Basso.aiff",
            Tone::Boot => "/System/Library/Sounds/Morse.aiff",
        }
    }

    /// Number of terminal bells for the fallback path — enough to tell the
    /// tones apart by ear.
    fn bell_count(self) -> usize {
        match self {
            Tone::On | Tone::Confirm => 1,
            Tone::Off => 2,
            Tone::Error => 3,
            Tone::Boot => 2,
        }
    }
}

fn afplay_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("afplay")
            .arg("-h")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    })
}

/// Play `tone` unless `enabled` is false. Never blocks and never fails: sound
/// is a nicety, not something worth interrupting a transfer for.
pub fn play(enabled: bool, tone: Tone) {
    if !enabled {
        return;
    }
    // Everything — including probing for afplay, which spawns a process —
    // happens off the UI thread.
    std::thread::spawn(move || {
        let file = tone.sound_file();
        if afplay_available() && Path::new(file).is_file() {
            let _ = Command::new("afplay")
                .args(["-v", "0.35", file])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            return;
        }
        let mut out = std::io::stdout();
        for i in 0..tone.bell_count() {
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(90));
            }
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
        }
    });
}
