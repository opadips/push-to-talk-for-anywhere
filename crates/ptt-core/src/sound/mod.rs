//! The talk / release cues (plan §1: "optional sound cues on talk start and
//! stop", §9 M5).
//!
//! Two short recordings are embedded in the binary: `talk_start.wav` plays
//! when the microphone opens (the bound input went down) and `talk_stop.wav`
//! when it closes again — after the release delay, so the cue means "you are
//! muted now", not just "you let go". `[sounds] volume` scales them.
//!
//! Like the microphone and the input hooks, playback sits behind a trait so
//! the engine can be tested with a fake ([`SoundPlayer`]); the Windows
//! implementation lives in [`winmm`]. Everything that is not a Windows call —
//! reading the WAV and applying the volume — is plain code, tested here.

#[cfg(windows)]
pub mod winmm;

/// Which of the two cues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// The microphone opened: the bound input went down.
    Start,
    /// The microphone closed again.
    Stop,
}

/// Plays a cue. `play` is called from the engine's worker thread, so it must
/// return at once — never wait for the sound to finish (plan §7: no audio
/// work on the hook thread, and none that can stall the mute either).
pub trait SoundPlayer: Send {
    /// `volume` is `[sounds] volume`: 0.0 (silent) to 1.0 (as recorded).
    fn play(&self, cue: Cue, volume: f32);
}

/// `assets/sounds/talk_start.wav` — from `source/pushing-sound.mp3`.
pub const START_WAV: &[u8] = include_bytes!("../../../../assets/sounds/talk_start.wav");
/// `assets/sounds/talk_stop.wav` — from `source/leaving-sound.mp3`.
pub const STOP_WAV: &[u8] = include_bytes!("../../../../assets/sounds/talk_stop.wav");

/// The recording behind a cue.
pub fn wav_for(cue: Cue) -> &'static [u8] {
    match cue {
        Cue::Start => START_WAV,
        Cue::Stop => STOP_WAV,
    }
}

/// A copy of `wav` with every sample multiplied by `volume` (clamped to
/// 0.0–1.0). The amplitude scales linearly: 0.5 is half the recorded
/// amplitude, about 6 dB quieter.
///
/// Only what the embedded recordings are is accepted: a RIFF/WAVE file with
/// 16-bit PCM samples. Anything else yields `None`.
pub fn scale_wav(wav: &[u8], volume: f32) -> Option<Vec<u8>> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let gain = if volume.is_nan() {
        0.0
    } else {
        volume.clamp(0.0, 1.0)
    };

    let mut pcm16 = false;
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
            let bits = u16::from_le_bytes(wav[body + 14..body + 16].try_into().ok()?);
            pcm16 = tag == 1 && bits == 16;
        } else if id == b"data" {
            data = Some(body..end);
            break;
        }
        // Chunks are padded to an even size.
        at = body.saturating_add(size).saturating_add(size & 1);
    }
    let data = data.filter(|_| pcm16)?;

    let mut scaled = wav.to_vec();
    for pair in scaled[data].chunks_exact_mut(2) {
        let sample = i16::from_le_bytes([pair[0], pair[1]]);
        let value = (f32::from(sample) * gain).round() as i16;
        pair.copy_from_slice(&value.to_le_bytes());
    }
    Some(scaled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples of a 16-bit PCM WAV, past the 44-byte header our files have.
    fn samples(wav: &[u8]) -> Vec<i16> {
        wav[44..]
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect()
    }

    fn peak(wav: &[u8]) -> i32 {
        samples(wav)
            .iter()
            .map(|sample| i32::from(*sample).abs())
            .max()
            .unwrap()
    }

    #[test]
    fn both_recordings_are_short_16_bit_wav_files_with_audible_content() {
        for (name, wav) in [("start", START_WAV), ("stop", STOP_WAV)] {
            let scaled = scale_wav(wav, 1.0).unwrap_or_else(|| panic!("{name} is 16-bit PCM"));
            assert_eq!(scaled, wav, "{name}: full volume leaves the samples alone");
            // 48 kHz mono: well under half a second, and not silence.
            let length = samples(wav).len();
            assert!((100..24_000).contains(&length), "{name}: {length} samples");
            assert!(peak(wav) > 1000, "{name} is audible");
        }
    }

    #[test]
    fn the_cues_map_to_their_own_recording() {
        assert_eq!(wav_for(Cue::Start), START_WAV);
        assert_eq!(wav_for(Cue::Stop), STOP_WAV);
        assert_ne!(START_WAV, STOP_WAV);
    }

    #[test]
    fn volume_scales_the_amplitude_linearly_and_keeps_the_file_layout() {
        let full = peak(START_WAV);
        let half = scale_wav(START_WAV, 0.5).unwrap();
        assert_eq!(half.len(), START_WAV.len(), "same size: header untouched");
        assert_eq!(&half[..44], &START_WAV[..44]);
        let halved = peak(&half);
        assert!((halved - full / 2).abs() <= 1, "{halved} vs {full}/2");
    }

    #[test]
    fn zero_volume_is_silence_and_out_of_range_values_are_clamped() {
        assert_eq!(peak(&scale_wav(STOP_WAV, 0.0).unwrap()), 0);
        assert_eq!(scale_wav(STOP_WAV, 7.0).unwrap(), STOP_WAV);
        assert_eq!(peak(&scale_wav(STOP_WAV, -1.0).unwrap()), 0);
        assert_eq!(peak(&scale_wav(STOP_WAV, f32::NAN).unwrap()), 0);
    }

    #[test]
    fn anything_that_is_not_16_bit_pcm_wav_is_refused() {
        assert!(scale_wav(b"", 1.0).is_none());
        assert!(scale_wav(b"not a wav file at all", 1.0).is_none());
        // Right container, 8-bit samples.
        let mut eight_bit = START_WAV.to_vec();
        eight_bit[34..36].copy_from_slice(&8u16.to_le_bytes());
        assert!(scale_wav(&eight_bit, 1.0).is_none());
        // A header with no data chunk.
        assert!(scale_wav(&START_WAV[..36], 1.0).is_none());
    }

    #[test]
    fn a_truncated_data_chunk_is_scaled_as_far_as_it_goes() {
        let cut = &START_WAV[..START_WAV.len() - 3];
        let scaled = scale_wav(cut, 0.5).unwrap();
        assert_eq!(scaled.len(), cut.len());
    }
}
