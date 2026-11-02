//! Opus as Simple Voice Chat uses it: 48 kHz mono, 20 ms frames.

use crate::opus::{Application, OpusDecoder, OpusEncoder};
use crate::payload::Codec;
use crate::{FRAME_SIZE, MAX_OPUS_PAYLOAD, SAMPLE_RATE};

pub struct Encoder {
    inner: OpusEncoder,
    out: Vec<u8>,
}

impl Encoder {
    pub fn new(codec: Codec) -> Result<Self, &'static str> {
        let app = match codec {
            Codec::Voip => Application::Voip,
            Codec::Audio => Application::Audio,
            Codec::RestrictedLowDelay => Application::RestrictedLowDelay,
        };
        let inner = OpusEncoder::new(SAMPLE_RATE as i32, 1, app)?;
        Ok(Encoder { inner, out: vec![0; MAX_OPUS_PAYLOAD] })
    }

    /// One frame of `FRAME_SIZE` samples to an Opus packet.
    pub fn encode(&mut self, frame: &[i16]) -> Result<Vec<u8>, &'static str> {
        if frame.len() != FRAME_SIZE {
            return Err("a frame is 960 samples");
        }
        let n = self.inner.encode_i16(frame, FRAME_SIZE, &mut self.out)?;
        Ok(self.out[..n].to_vec())
    }
}

/// One per speaker: Opus decoding carries state from frame to frame.
pub struct Decoder {
    inner: OpusDecoder,
    pcm: Vec<f32>,
}

impl Decoder {
    pub fn new() -> Result<Self, &'static str> {
        Ok(Decoder { inner: OpusDecoder::new(SAMPLE_RATE as i32, 1)?, pcm: vec![0.0; FRAME_SIZE] })
    }

    /// An Opus packet to a frame of samples in -1..1. An empty packet is a
    /// lost frame, which the decoder conceals.
    pub fn decode(&mut self, packet: &[u8]) -> Result<&[f32], &'static str> {
        let n = self.inner.decode(packet, FRAME_SIZE, &mut self.pcm)?;
        Ok(&self.pcm[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize) -> Vec<i16> {
        (0..frames * FRAME_SIZE)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let s = (t * 440.0 * std::f32::consts::TAU).sin() * 0.4
                    + (t * 1250.0 * std::f32::consts::TAU).sin() * 0.2;
                (s * 32767.0) as i16
            })
            .collect()
    }

    #[test]
    fn voice_frames_round_trip() {
        let input = tone(50);
        let mut enc = Encoder::new(Codec::Voip).unwrap();
        let mut dec = Decoder::new().unwrap();
        let mut out = Vec::new();
        for frame in input.chunks(FRAME_SIZE) {
            let packet = enc.encode(frame).unwrap();
            assert!(!packet.is_empty() && packet.len() <= MAX_OPUS_PAYLOAD);
            out.extend_from_slice(dec.decode(&packet).unwrap());
        }
        assert_eq!(out.len(), input.len());

        // Opus delays the signal by a few ms; find the best alignment and
        // require a clearly correlated result after the first frames.
        let x: Vec<f32> = input.iter().map(|&s| s as f32 / 32768.0).collect();
        let best = (0..480)
            .map(|lag| {
                let (mut num, mut ex, mut ey) = (0.0f64, 0.0f64, 0.0f64);
                for i in 10 * FRAME_SIZE..x.len() - 480 {
                    let a = x[i] as f64;
                    let b = out[i + lag] as f64;
                    num += a * b;
                    ex += a * a;
                    ey += b * b;
                }
                num / (ex * ey).sqrt()
            })
            .fold(f64::MIN, f64::max);
        assert!(best > 0.9, "correlation {best}");
    }

    #[test]
    fn lost_frame_is_concealed() {
        let mut enc = Encoder::new(Codec::Voip).unwrap();
        let mut dec = Decoder::new().unwrap();
        for frame in tone(5).chunks(FRAME_SIZE) {
            dec.decode(&enc.encode(frame).unwrap()).unwrap();
        }
        assert_eq!(dec.decode(&[]).unwrap().len(), FRAME_SIZE);
    }

    #[test]
    fn wrong_frame_size_is_refused() {
        assert!(Encoder::new(Codec::Voip).unwrap().encode(&[0; 100]).is_err());
    }
}
