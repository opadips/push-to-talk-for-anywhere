//! Linux playback of the cues through PulseAudio's "simple" API.
//!
//! One small worker thread owns the playing, so [`SoundPlayer::play`] only
//! drops a message in a channel and returns (the same shape as [`winmm`]).
//! For every cue it scales the embedded WAV to the volume, opens a short-lived
//! playback connection to the default sink — which is a PipeWire sandbox too,
//! since Pipewire exposes PulseAudio's API through `pipewire-pulse` — writes
//! the 16-bit samples, waits for them to drain and goes back to waiting. Cues
//! play one after the other; a quick tap therefore sounds "start", "stop".
//!
//! The whole binary is `S16LE` bytes (that is what [`super::scale_wav`]
//! produces), so the stream format is pinned to little-endian rather than
//! native — every machine this targets is little-endian anyway.

use libpulse_binding as pulse;
use libpulse_simple_binding::Simple;
use std::sync::mpsc::{channel, Sender};
use tracing::warn;

use super::{scale_wav, wav_for, Cue, SoundPlayer};

/// The samples of a 16-bit PCM WAV file, plus the stream shape needed to play
/// them: `channel_count` and `sample_rate` come from the `fmt ` chunk,
/// `samples` is the payload of the `data` chunk.
pub struct WavStream<'a> {
    /// 1 for mono, 2 for stereo, and so on — played as-is.
    pub channel_count: u8,
    /// Samples per second per channel, in Hertz.
    pub sample_rate: u32,
    /// The raw little-endian 16-bit samples of the `data` chunk.
    pub samples: &'a [u8],
}

/// Reads `fmt ` and `data` out of a WAV image. Only a RIFF/WAVE container
/// with a 16-bit PCM `fmt ` chunk is accepted — like [`super::scale_wav`], and
/// unlike it this borrows instead of copying, so the samples stay where they
/// are until the connection is closed.
pub fn wav_pcm(wav: &[u8]) -> Option<WavStream<'_>> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }

    let mut format = None;
    let mut data = None;
    let mut at = 12;
    while at + 8 <= wav.len() {
        let id = &wav[at..at + 4];
        let size = u32::from_le_bytes(wav[at + 4..at + 8].try_into().ok()?) as usize;
        let body = at + 8;
        // A chunk that claims more than the file holds is cut at the end.
        let end = body.saturating_add(size).min(wav.len());
        if id == b"fmt " && end - body >= 16 {
            let tag = u16::from_le_bytes(wav[body..body + 2].try_into().ok()?);
            let channel_count = u16::from_le_bytes(wav[body + 2..body + 4].try_into().ok()?);
            let sample_rate = u32::from_le_bytes(wav[body + 4..body + 8].try_into().ok()?);
            let bits = u16::from_le_bytes(wav[body + 14..body + 16].try_into().ok()?);
            format = (tag == 1 && bits == 16)
                .then_some((channel_count, sample_rate))
                .filter(|(channels, _)| *channels > 0)
        } else if id == b"data" {
            data = Some(body..end);
            break;
        }
        // Chunks are padded to an even size.
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    let (channel_count, sample_rate) = format?;
    let samples = wav.get(data?)?;
    Some(WavStream {
        channel_count: u8::try_from(channel_count).ok()?,
        sample_rate,
        samples,
    })
}

/// The Linux [`SoundPlayer`]. Dropping it ends the worker thread.
pub struct PulsePlayer {
    cues: Sender<(Cue, f32)>,
}

impl PulsePlayer {
    pub fn new() -> Self {
        let (cues, queue) = channel::<(Cue, f32)>();
        let spawned = std::thread::Builder::new()
            .name("ptt-sound".into())
            .spawn(move || {
                let mut failed_before = false;
                // Ends when the player is dropped and the channel closes.
                while let Ok((cue, volume)) = queue.recv() {
                    // Silence needs no playback (and no connection).
                    if volume <= 0.0 {
                        continue;
                    }
                    let Some(scaled) = scale_wav(wav_for(cue), volume) else {
                        continue;
                    };
                    let Some(stream) = wav_pcm(&scaled) else {
                        continue;
                    };
                    // A fresh connection per cue: the simple API has no way to
                    // reuse one without instantly playing silence, and the
                    // cost of a connect is a few milliseconds.
                    let spec = pulse::sample::Spec {
                        format: pulse::sample::Format::S16le,
                        channels: stream.channel_count,
                        rate: stream.sample_rate,
                    };
                    let connection = Simple::new(
                        None, // default server
                        "push-to-talk-for-anywhere",
                        pulse::stream::Direction::Playback,
                        None, // default sink
                        "push-to-talk cue",
                        &spec,
                        None, // default channel map
                        None, // default buffering attributes
                    );
                    match connection {
                        Ok(conn) => {
                            // `write` and `drain` both return only after the
                            // data is accepted/played, so `samples` outlives
                            // its use and `drain` keeps the cues in order.
                            match conn.write(stream.samples).and_then(|()| conn.drain()) {
                                Ok(()) => {}
                                Err(error) if !failed_before => {
                                    failed_before = true;
                                    warn!("the {cue:?} sound could not be played: {error}");
                                }
                                _ => {}
                            }
                        }
                        Err(error) if !failed_before => {
                            // Once is enough: a machine without an output
                            // device would otherwise log on every key press.
                            failed_before = true;
                            warn!("the {cue:?} sound has nowhere to play: {error}");
                        }
                        _ => {}
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

impl Default for PulsePlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl SoundPlayer for PulsePlayer {
    fn play(&self, cue: Cue, volume: f32) {
        let _ = self.cues.send((cue, volume));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded cues (`mod.rs` tests already cover this, but these pin the
    /// stream shape the playback connection is built from).
    #[test]
    fn the_embedded_cues_are_mono_48khz_16bit_pcm() {
        for (name, wav) in [
            ("start", super::super::START_WAV),
            ("stop", super::super::STOP_WAV),
        ] {
            let stream =
                wav_pcm(wav).unwrap_or_else(|| panic!("{name} is a 16-bit PCM WAV we can stream"));
            assert_eq!(stream.channel_count, 1, "{name}: mono");
            assert_eq!(stream.sample_rate, 48_000, "{name}: 48 kHz");
            // The 44-byte header is the whole container: nothing left over.
            assert_eq!(
                stream.samples.len() + 44,
                wav.len(),
                "{name}: no extra chunks"
            );
        }
    }

    #[test]
    fn anything_that_is_not_16_bit_pcm_wav_is_refused() {
        assert!(wav_pcm(b"").is_none());
        assert!(wav_pcm(b"not a wav file at all").is_none());
        // Still a WAV, but an 8-bit one.
        let mut eight_bit = super::super::START_WAV.to_vec();
        eight_bit[34..36].copy_from_slice(&8u16.to_le_bytes());
        assert!(wav_pcm(&eight_bit).is_none());
        // A header with no data chunk.
        assert!(wav_pcm(&super::super::START_WAV[..36]).is_none());
    }

    #[test]
    fn a_truncated_data_chunk_is_read_as_far_as_it_goes() {
        let cut = &super::super::START_WAV[..super::super::START_WAV.len() - 3];
        let stream = wav_pcm(cut).unwrap();
        assert_eq!(stream.samples.len() + 44, cut.len());
    }
}
