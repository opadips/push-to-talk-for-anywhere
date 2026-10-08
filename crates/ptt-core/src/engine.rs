//! Engine wiring: input events -> state machine -> audio, with the audio call
//! on a worker thread separate from the hook thread (plan §7 and M3).
