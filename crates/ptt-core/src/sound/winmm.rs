//! Windows playback of the cues through `PlaySoundW` (winmm).
//!
//! One small worker thread owns the playing, so [`SoundPlayer::play`] only
//! drops a message in a channel and returns. The thread scales the embedded
//! WAV to the volume, plays it synchronously from memory (so the buffer stays
//! valid exactly as long as it is needed) and goes back to waiting. Cues are
//! played one after the other; a quick tap therefore sounds "start", "stop".
//! Playing goes to the default output device.

use super::{scale_wav, wav_for, Cue, SoundPlayer};
use std::sync::mpsc::{channel, Sender};
use tracing::warn;
use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC};

/// The Windows [`SoundPlayer`]. Dropping it ends the worker thread.
pub struct WinmmPlayer {
    cues: Sender<(Cue, f32)>,
}

impl WinmmPlayer {
    pub fn new() -> Self {
        let (cues, queue) = channel::<(Cue, f32)>();
        let spawned = std::thread::Builder::new()
            .name("ptt-sound".into())
            .spawn(move || {
                let mut failed_before = false;
                // Ends when the player is dropped and the channel closes.
                while let Ok((cue, volume)) = queue.recv() {
                    // Silence needs no playback.
                    if volume <= 0.0 {
                        continue;
                    }
                    let Some(wav) = scale_wav(wav_for(cue), volume) else {
                        continue;
                    };
                    // SAFETY: `wav` outlives the call — SND_SYNC returns only
                    // once the sound has finished — and with SND_MEMORY the
                    // "name" is a pointer to the WAV image in memory.
                    let played = unsafe {
                        PlaySoundW(
                            PCWSTR(wav.as_ptr().cast()),
                            None,
                            SND_MEMORY | SND_SYNC | SND_NODEFAULT,
                        )
                    };
                    if !played.as_bool() && !failed_before {
                        // Once is enough: a machine without an output device
                        // would otherwise log on every key press.
                        failed_before = true;
                        warn!("the {cue:?} sound could not be played (is there an output device?)");
                    }
                }
            });
        if let Err(error) = spawned {
            // No sound is better than no push-to-talk: the sender simply has
            // nobody listening, and `play` ignores the failed send.
            warn!("cannot start the sound thread: {error}");
        }
        Self { cues }
    }
}

impl Default for WinmmPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl SoundPlayer for WinmmPlayer {
    fn play(&self, cue: Cue, volume: f32) {
        let _ = self.cues.send((cue, volume));
    }
}
