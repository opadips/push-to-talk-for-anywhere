//! macOS playback of the cues through rodio (which uses CoreAudio under the
//! hood).
//!
//! One small worker thread owns the playing, so [`SoundPlayer::play`] only
//! drops a message in a channel and returns (the same shape as [`winmm`]).
//! The default output sink is opened once and kept for the thread's whole
//! life — a `cpal` stream, so it must outlive every player — and each cue is
//! played by appending the scaled WAV to the sink's mixer and blocking on the
//! player. Cues therefore run one after the other; a quick tap sounds
//! "start", "stop".

use super::{scale_wav, wav_for, Cue, SoundPlayer};
use rodio::{DeviceSinkBuilder, MixerDeviceSink};
use std::io::Cursor;
use std::sync::mpsc::{channel, Sender};
use tracing::warn;

/// The macOS [`SoundPlayer`]. Dropping it ends the worker thread.
pub struct RodioPlayer {
    cues: Sender<(Cue, f32)>,
}

impl RodioPlayer {
    pub fn new() -> Self {
        let (cues, queue) = channel::<(Cue, f32)>();
        let spawned = std::thread::Builder::new()
            .name("ptt-sound".into())
            .spawn(move || {
                let mut sink = None::<MixerDeviceSink>;
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
                    // One output connection for the thread's life; opening it
                    // fails on a machine with no audio output, which downgrades
                    // to silence.
                    if sink.is_none() {
                        match DeviceSinkBuilder::open_default_sink() {
                            Ok(mut opened) => {
                                // No "Dropping DeviceSink…" chatter at exit.
                                opened.log_on_drop(false);
                                sink = Some(opened);
                            }
                            Err(error) if !failed_before => {
                                // Once is enough: a machine without an output
                                // device would otherwise log on every press.
                                failed_before = true;
                                warn!("the {cue:?} sound has nowhere to play: {error}");
                            }
                            _ => {}
                        }
                    }
                    let Some(opened) = sink.as_mut() else {
                        continue;
                    };
                    // The WAV is a plain 16-bit PCM image; `Decoder::new` rates
                    // it through the bundled WAV decoder, and `Cursor` keeps
                    // the bytes alive exactly as long as they are needed.
                    match rodio::play(opened.mixer(), Cursor::new(wav)) {
                        Ok(player) => player.sleep_until_end(),
                        Err(error) => {
                            if !failed_before {
                                failed_before = true;
                                warn!("the {cue:?} sound could not be played: {error}");
                            }
                            // The output device may have come back later, so
                            // drop the possibly-dead stream and reopen.
                            sink = None;
                        }
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

impl Default for RodioPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl SoundPlayer for RodioPlayer {
    fn play(&self, cue: Cue, volume: f32) {
        let _ = self.cues.send((cue, volume));
    }
}
