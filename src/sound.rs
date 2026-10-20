//! Deterrent sounds: built-in clips, imported clips and the playback thread.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use rodio::Source;

pub struct Builtin {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub data: &'static [u8],
}

pub const SILENT: &str = "silent";

pub const BUILTIN: &[Builtin] = &[
    Builtin {
        id: "warning",
        label: "Warning: Cat on Keyboard",
        icon: "audio-input-microphone-symbolic",
        data: include_bytes!("../assets/sounds/warning.wav"),
    },
    Builtin { id: "alarm", label: "Alarm", icon: "alarm-symbolic", data: include_bytes!("../assets/sounds/alarm.wav") },
    Builtin {
        id: "meow",
        label: "Meow",
        icon: "emoji-nature-symbolic",
        data: include_bytes!("../assets/sounds/meow.wav"),
    },
    Builtin {
        id: "bark",
        label: "Dog Bark",
        icon: "emoji-nature-symbolic",
        data: include_bytes!("../assets/sounds/bark.wav"),
    },
    Builtin {
        id: "vacuum",
        label: "Vacuum Cleaner",
        icon: "weather-windy-symbolic",
        data: include_bytes!("../assets/sounds/vacuum.wav"),
    },
    Builtin {
        id: "bubbles",
        label: "Bubbles",
        icon: "weather-showers-scattered-symbolic",
        data: include_bytes!("../assets/sounds/bubbles.wav"),
    },
    Builtin {
        id: "purr",
        label: "Purr",
        icon: "emoji-nature-symbolic",
        data: include_bytes!("../assets/sounds/purr.wav"),
    },
    Builtin {
        id: "lofi",
        label: "Lo-fi",
        icon: "folder-music-symbolic",
        data: include_bytes!("../assets/sounds/lofi.wav"),
    },
];

/// Imported clips stay short and small: they play on every blocked keystroke.
pub const MAX_IMPORT_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_IMPORT_LENGTH: Duration = Duration::from_secs(15);
const FILE_PREFIX: &str = "file:";

pub fn custom_dir() -> PathBuf {
    gtk::glib::user_data_dir().join("catlock").join("sounds")
}

pub fn custom_sounds() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(custom_dir())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

pub fn custom_id(name: &str) -> String {
    format!("{FILE_PREFIX}{name}")
}

pub fn custom_name(id: &str) -> Option<&str> {
    id.strip_prefix(FILE_PREFIX)
}

/// Copies an audio file into the sounds directory after checking that it
/// decodes and fits the size and length limits. Returns the new sound id.
pub fn import(source: &Path) -> Result<String, String> {
    let size = std::fs::metadata(source).map_err(|e| e.to_string())?.len();
    if size > MAX_IMPORT_BYTES {
        return Err("The clip is larger than 5 MB.".into());
    }
    let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
    let decoder =
        rodio::Decoder::new(Cursor::new(bytes.clone())).map_err(|_| "This is not an audio file Catlock can play.")?;
    if decoder.total_duration().is_some_and(|d| d > MAX_IMPORT_LENGTH) {
        return Err("The clip is longer than 15 seconds.".into());
    }
    let name = source.file_name().and_then(|n| n.to_str()).ok_or("The file name is not valid text.")?;
    std::fs::create_dir_all(custom_dir()).map_err(|e| e.to_string())?;
    std::fs::write(custom_dir().join(name), bytes).map_err(|e| e.to_string())?;
    Ok(custom_id(name))
}

pub fn remove_custom(id: &str) {
    if let Some(name) = custom_name(id) {
        let _ = std::fs::remove_file(custom_dir().join(name));
    }
}

fn load(id: &str) -> Option<Vec<u8>> {
    match custom_name(id) {
        Some(name) => std::fs::read(custom_dir().join(name)).ok(),
        None => BUILTIN.iter().find(|b| b.id == id).map(|b| b.data.to_vec()),
    }
}

struct Request {
    id: String,
    volume: f32,
    /// Previews interrupt; deterrents skip while one is still playing so a
    /// cat walking across the keys does not stack dozens of clips.
    interrupt: bool,
}

#[derive(Clone)]
pub struct Player {
    tx: mpsc::Sender<Request>,
}

impl Player {
    /// Starts the playback thread. Audio is optional: without an output
    /// device every request is silently dropped.
    /// Starts the playback thread. The output stream is opened only while
    /// something plays: an always-open stream keeps the sound card awake all
    /// day. Without an output device every request is silently dropped.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<Request>();
        std::thread::Builder::new()
            .name("catlock-sound".into())
            .spawn(move || {
                let mut sink: Option<rodio::MixerDeviceSink> = None;
                let mut current: Option<rodio::Player> = None;
                loop {
                    let req = match rx.recv_timeout(Duration::from_secs(5)) {
                        Ok(req) => req,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if current.as_ref().is_none_or(|p| p.empty()) {
                                current = None;
                                sink = None;
                            }
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    };
                    if current.as_ref().is_some_and(|p| !p.empty()) && !req.interrupt {
                        continue;
                    }
                    let Some(bytes) = load(&req.id) else { continue };
                    let Ok(source) = rodio::Decoder::new(Cursor::new(bytes)) else { continue };
                    if sink.is_none() {
                        sink = rodio::DeviceSinkBuilder::from_default_device()
                            .and_then(|b| b.with_error_callback(|_| {}).open_sink_or_fallback())
                            .ok();
                    }
                    let Some(sink) = sink.as_ref() else { continue };
                    let player = rodio::Player::connect_new(sink.mixer());
                    player.set_volume(req.volume);
                    player.append(source);
                    current = Some(player);
                }
            })
            .expect("spawn sound thread");
        Self { tx }
    }

    pub fn deter(&self, id: &str, volume: f32) {
        if id != SILENT {
            let _ = self.tx.send(Request { id: id.into(), volume, interrupt: false });
        }
    }

    pub fn preview(&self, id: &str, volume: f32) {
        if id != SILENT {
            let _ = self.tx.send(Request { id: id.into(), volume, interrupt: true });
        }
    }
}
