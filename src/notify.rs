//! Attention signal: the "too-tah" chime from `sound/too-tah.wav`, embedded
//! in the binary so an installed blush doesn't need the repo next to it.
//! Best effort: failures are silent.
//!
//! Windows plays it straight from memory through `PlaySound` (winmm).
//! macOS (`afplay`) and Linux (`paplay`, falling back to `aplay`) play a
//! copy written once to the temp directory.

/// The chime, baked into the binary.
static SOUND: &[u8] = include_bytes!("../sound/too-tah.wav");

/// Play the chime without blocking.
pub fn chime() {
    play();
}

#[cfg(windows)]
fn play() {
    #[link(name = "winmm")]
    unsafe extern "system" {
        fn PlaySoundW(sound: *const u8, module: *mut core::ffi::c_void, flags: u32) -> i32;
    }
    const SND_ASYNC: u32 = 0x0001;
    const SND_NODEFAULT: u32 = 0x0002;
    const SND_MEMORY: u32 = 0x0004;
    // SAFETY: SOUND is 'static, so the buffer outlives the async playback;
    // SND_MEMORY tells winmm the pointer is WAV data, not a wide string.
    unsafe {
        PlaySoundW(SOUND.as_ptr(), std::ptr::null_mut(), SND_ASYNC | SND_NODEFAULT | SND_MEMORY);
    }
}

#[cfg(not(windows))]
fn play() {
    use std::process::{Command, Stdio};

    std::thread::spawn(|| {
        let Some(path) = sound_file() else { return };
        let players: &[&str] = if cfg!(target_os = "macos") { &["afplay"] } else { &["paplay", "aplay"] };
        for player in players {
            let status = Command::new(player)
                .arg(&path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if status.is_ok_and(|s| s.success()) {
                return;
            }
        }
    });
}

/// The chime on disk for players that need a path; written on first use.
#[cfg(not(windows))]
fn sound_file() -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!("blush-too-tah-{}.wav", SOUND.len()));
    let fresh = std::fs::metadata(&path).is_ok_and(|m| m.len() == SOUND.len() as u64);
    if !fresh {
        std::fs::write(&path, SOUND).ok()?;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_sound_is_a_wav() {
        assert_eq!(&SOUND[..4], b"RIFF");
        assert_eq!(&SOUND[8..12], b"WAVE");
    }
}
