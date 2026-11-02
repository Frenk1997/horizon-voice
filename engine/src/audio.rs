//! Sound in and out: the microphone as 20 ms frames at 48 kHz, and the other
//! players' voices mixed to the speakers.
//!
//! The mixing and resampling are plain code with tests; only [`Capture`] and
//! [`Playback`] touch the sound card (cpal), each on a thread of its own that
//! owns its stream.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::buf::Uuid;
use crate::{FRAME_SIZE, SAMPLE_RATE};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Linear resampling of a mono stream from one rate to another, carrying the
/// position across calls.
pub struct Resampler {
    step: f64,
    pos: f64,
    last: f32,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Resampler { step: from as f64 / to as f64, pos: 0.0, last: 0.0 }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.step == 1.0 {
            out.extend_from_slice(input);
            return;
        }
        // `pos` indexes the input, -1 being `last` (the previous call's final
        // sample); it interpolates between floor(pos) and the sample after.
        while self.pos < input.len() as f64 - 1.0 {
            let i = self.pos.floor();
            let frac = (self.pos - i) as f32;
            let a = if i < 0.0 { self.last } else { input[i as usize] };
            let b = input[(i + 1.0) as usize];
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        if let Some(&l) = input.last() {
            self.last = l;
        }
        self.pos -= input.len() as f64;
    }
}

/// Speech is played once this many frames wait, so network jitter doesn't cut
/// it up; more than `MAX_QUEUED` and the oldest are dropped to keep latency low.
const START_FRAMES: usize = 2;
const MAX_QUEUED: usize = 10;

struct Speaker {
    queue: VecDeque<[f32; 2]>,
    playing: bool,
}

/// The other players' voices, each with its own queue, mixed to stereo at
/// 48 kHz.
#[derive(Default)]
pub struct Mixer {
    speakers: HashMap<Uuid, Speaker>,
    pub volume: f32,
}

impl Mixer {
    pub fn new() -> Self {
        Mixer { speakers: HashMap::new(), volume: 1.0 }
    }

    /// A decoded frame from `sender`, with the left and right gains for where
    /// they stand.
    pub fn push(&mut self, sender: Uuid, mono: &[f32], gains: [f32; 2]) {
        let s = self.speakers.entry(sender).or_insert_with(|| Speaker { queue: VecDeque::new(), playing: false });
        s.queue.extend(mono.iter().map(|&x| [x * gains[0], x * gains[1]]));
        let max = MAX_QUEUED * FRAME_SIZE;
        if s.queue.len() > max {
            let drop = s.queue.len() - max;
            s.queue.drain(..drop);
        }
        if s.queue.len() >= START_FRAMES * FRAME_SIZE {
            s.playing = true;
        }
    }

    /// Whether `sender` is being heard now (for the HUD).
    pub fn is_playing(&self, sender: Uuid) -> bool {
        self.speakers.get(&sender).is_some_and(|s| s.playing)
    }

    pub fn talking(&self) -> Vec<Uuid> {
        self.speakers.iter().filter(|(_, s)| s.playing).map(|(&id, _)| id).collect()
    }

    /// The next stereo sample at 48 kHz.
    pub fn next(&mut self) -> [f32; 2] {
        let mut out = [0.0f32; 2];
        for s in self.speakers.values_mut() {
            if !s.playing {
                continue;
            }
            match s.queue.pop_front() {
                Some([l, r]) => {
                    out[0] += l;
                    out[1] += r;
                }
                // Ran dry: wait to fill up again rather than stutter.
                None => s.playing = false,
            }
        }
        [(out[0] * self.volume).clamp(-1.0, 1.0), (out[1] * self.volume).clamp(-1.0, 1.0)]
    }

    pub fn forget(&mut self, sender: Uuid) {
        self.speakers.remove(&sender);
    }

    pub fn clear(&mut self) {
        self.speakers.clear();
    }
}

/// Pulls 48 kHz stereo from the mixer at the device's rate.
struct OutputResampler {
    step: f64,
    pos: f64,
    a: [f32; 2],
    b: [f32; 2],
}

impl OutputResampler {
    fn new(device_rate: u32) -> Self {
        OutputResampler { step: SAMPLE_RATE as f64 / device_rate as f64, pos: 0.0, a: [0.0; 2], b: [0.0; 2] }
    }

    fn next(&mut self, mixer: &mut Mixer) -> [f32; 2] {
        while self.pos >= 1.0 {
            self.a = self.b;
            self.b = mixer.next();
            self.pos -= 1.0;
        }
        let t = self.pos as f32;
        let out = [self.a[0] + (self.b[0] - self.a[0]) * t, self.a[1] + (self.b[1] - self.a[1]) * t];
        self.pos += self.step;
        out
    }
}

/// How loud a voice is at `listener` from `source` (1 near, 0 at `range`), and
/// how it pans: `yaw` is Minecraft's (0 faces +Z, 90 faces -X).
pub fn spatial_gains(listener: [f64; 3], yaw: f32, source: [f64; 3], range: f32) -> [f32; 2] {
    let d = [source[0] - listener[0], source[1] - listener[1], source[2] - listener[2]];
    let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() as f32;
    let volume = if range > 0.0 { (1.0 - dist / range).clamp(0.0, 1.0) } else { 1.0 };
    let flat = (d[0] * d[0] + d[2] * d[2]).sqrt();
    // The listener's right hand: facing +Z (yaw 0) it points to -X.
    let yaw = (yaw as f64).to_radians();
    let right = [-yaw.cos(), -yaw.sin()];
    let mut pan = if flat > 1e-6 { ((d[0] * right[0] + d[2] * right[1]) / flat) as f32 } else { 0.0 };
    // Sound from far above or below pans less.
    let vertical = ((d[1].abs() / 32.0) as f32).min(1.0);
    pan *= 1.0 - vertical;
    // Never silence one ear completely: speech from the side should still be
    // heard on both.
    let pan = pan * 0.8;
    let left = ((1.0 - pan) / 2.0).sqrt() * std::f32::consts::SQRT_2;
    let right = ((1.0 + pan) / 2.0).sqrt() * std::f32::consts::SQRT_2;
    [left * volume, right * volume]
}

/// The level of a frame in dBFS (-127 for silence), for voice activation.
pub fn level_db(frame: &[i16]) -> f32 {
    if frame.is_empty() {
        return -127.0;
    }
    let sum: f64 = frame.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum();
    let rms = (sum / frame.len() as f64).sqrt();
    if rms <= 0.0 { -127.0 } else { (20.0 * rms.log10()).max(-127.0) as f32 }
}

pub fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices().map(|d| d.filter_map(|d| d.name().ok()).collect()).unwrap_or_default()
}

pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices().map(|d| d.filter_map(|d| d.name().ok()).collect()).unwrap_or_default()
}

fn pick_device(input: bool, name: Option<&str>) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    if let Some(name) = name.filter(|n| !n.is_empty()) {
        let devices = if input { host.input_devices() } else { host.output_devices() };
        if let Some(d) = devices.map_err(|e| e.to_string())?.find(|d| d.name().is_ok_and(|n| n == name)) {
            return Ok(d);
        }
    }
    let d = if input { host.default_input_device() } else { host.default_output_device() };
    d.ok_or_else(|| if input { "no microphone".to_owned() } else { "no speakers".to_owned() })
}

/// Runs `build` on a thread of its own that keeps the stream alive until the
/// returned sender is dropped. cpal streams can't move between threads on
/// every platform.
fn stream_thread(
    name: &str,
    build: impl FnOnce() -> Result<cpal::Stream, String> + Send + 'static,
) -> Result<mpsc::Sender<()>, String> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let stream = match build().and_then(|s| s.play().map(|_| s).map_err(|e| e.to_string())) {
                Ok(s) => {
                    let _ = ready_tx.send(Ok(()));
                    s
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // Until the owner drops its sender.
            let _ = stop_rx.recv();
            drop(stream);
        })
        .map_err(|e| e.to_string())?;
    ready_rx.recv().map_err(|_| "audio thread died".to_owned())??;
    Ok(stop_tx)
}

/// The microphone, collected into 48 kHz mono frames.
pub struct Capture {
    samples: Arc<Mutex<VecDeque<f32>>>,
    _stop: mpsc::Sender<()>,
    pub device: String,
}

impl Capture {
    pub fn open(device: Option<&str>) -> Result<Capture, String> {
        let samples = Arc::new(Mutex::new(VecDeque::new()));
        let shared = samples.clone();
        let device = device.map(str::to_owned);
        let (name_tx, name_rx) = mpsc::channel();
        let stop = stream_thread("horizon-voice-mic", move || {
            let dev = pick_device(true, device.as_deref())?;
            let _ = name_tx.send(dev.name().unwrap_or_default());
            let supported = dev.default_input_config().map_err(|e| e.to_string())?;
            let config: StreamConfig = supported.config();
            match supported.sample_format() {
                SampleFormat::F32 => build_input::<f32>(&dev, &config, shared),
                SampleFormat::I16 => build_input::<i16>(&dev, &config, shared),
                SampleFormat::U16 => build_input::<u16>(&dev, &config, shared),
                other => Err(format!("microphone sample format {other:?} is not supported")),
            }
        })?;
        Ok(Capture { samples, _stop: stop, device: name_rx.try_recv().unwrap_or_default() })
    }

    /// The whole frames recorded since the last call.
    pub fn frames(&self) -> Vec<Vec<i16>> {
        let mut q = lock(&self.samples);
        let mut out = Vec::new();
        while q.len() >= FRAME_SIZE {
            out.push(q.drain(..FRAME_SIZE).map(|s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16).collect());
        }
        out
    }
}

fn build_input<T>(
    dev: &cpal::Device,
    config: &StreamConfig,
    samples: Arc<Mutex<VecDeque<f32>>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let mut resampler = Resampler::new(config.sample_rate.0, SAMPLE_RATE);
    let mut mono = Vec::new();
    let mut resampled = Vec::new();
    dev.build_input_stream(
        config,
        move |data: &[T], _| {
            mono.clear();
            mono.extend(data.chunks(channels).map(|c| c.iter().map(|&s| s.to_sample::<f32>()).sum::<f32>() / channels as f32));
            resampled.clear();
            resampler.process(&mono, &mut resampled);
            let mut q = lock(&samples);
            q.extend(resampled.iter().copied());
            // At most a second waits; older sound is stale.
            let max = SAMPLE_RATE as usize;
            if q.len() > max {
                let drop = q.len() - max;
                q.drain(..drop);
            }
        },
        |e| eprintln!("[horizon_voice] microphone error: {e}"),
        None,
    )
    .map_err(|e| e.to_string())
}

/// The speakers, playing what the shared mixer holds.
pub struct Playback {
    pub mixer: Arc<Mutex<Mixer>>,
    _stop: mpsc::Sender<()>,
}

impl Playback {
    pub fn open(device: Option<&str>, mixer: Arc<Mutex<Mixer>>) -> Result<Playback, String> {
        let shared = mixer.clone();
        let device = device.map(str::to_owned);
        let stop = stream_thread("horizon-voice-out", move || {
            let dev = pick_device(false, device.as_deref())?;
            let supported = dev.default_output_config().map_err(|e| e.to_string())?;
            let config: StreamConfig = supported.config();
            match supported.sample_format() {
                SampleFormat::F32 => build_output::<f32>(&dev, &config, shared),
                SampleFormat::I16 => build_output::<i16>(&dev, &config, shared),
                SampleFormat::U16 => build_output::<u16>(&dev, &config, shared),
                other => Err(format!("speaker sample format {other:?} is not supported")),
            }
        })?;
        Ok(Playback { mixer, _stop: stop })
    }
}

fn build_output<T>(dev: &cpal::Device, config: &StreamConfig, mixer: Arc<Mutex<Mixer>>) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels.max(1) as usize;
    let mut resampler = OutputResampler::new(config.sample_rate.0);
    dev.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let mut m = lock(&mixer);
            for frame in data.chunks_mut(channels) {
                let [l, r] = resampler.next(&mut m);
                match frame.len() {
                    1 => frame[0] = T::from_sample((l + r) * 0.5),
                    _ => {
                        frame[0] = T::from_sample(l);
                        frame[1] = T::from_sample(r);
                        for extra in &mut frame[2..] {
                            *extra = T::from_sample(0.0);
                        }
                    }
                }
            }
        },
        |e| eprintln!("[horizon_voice] speaker error: {e}"),
        None,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_keeps_the_rate_ratio_across_calls() {
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        let input: Vec<f32> = (0..441).map(|i| (i as f32 * 0.01).sin()).collect();
        for _ in 0..100 {
            r.process(&input, &mut out);
        }
        // 1 s of 44.1 kHz becomes about 1 s of 48 kHz.
        assert!((out.len() as i64 - 48_000).abs() < 10, "{}", out.len());
    }

    #[test]
    fn resampler_is_continuous_at_call_boundaries() {
        let mut r = Resampler::new(16_000, 48_000);
        let mut out = Vec::new();
        let ramp: Vec<f32> = (0..32).map(|i| i as f32).collect();
        r.process(&ramp[..16], &mut out);
        r.process(&ramp[16..], &mut out);
        for w in out.windows(2) {
            let step = w[1] - w[0];
            assert!((step - 1.0 / 3.0).abs() < 1e-4, "{step}");
        }
    }

    #[test]
    fn mixer_waits_for_two_frames_then_plays_and_mixes() {
        let mut m = Mixer::new();
        m.push(1, &[0.25; FRAME_SIZE], [1.0, 1.0]);
        assert_eq!(m.next(), [0.0, 0.0], "one frame is not enough yet");
        m.push(1, &[0.25; FRAME_SIZE], [1.0, 1.0]);
        m.push(2, &[0.5; FRAME_SIZE], [0.0, 1.0]);
        m.push(2, &[0.5; FRAME_SIZE], [0.0, 1.0]);
        assert_eq!(m.next(), [0.25, 0.75]);
        assert_eq!(m.talking().len(), 2);
        for _ in 0..2 * FRAME_SIZE {
            m.next();
        }
        assert!(!m.is_playing(1), "ran dry and stopped");
    }

    #[test]
    fn mixer_caps_latency() {
        let mut m = Mixer::new();
        for _ in 0..30 {
            m.push(1, &[0.1; FRAME_SIZE], [1.0, 1.0]);
        }
        assert_eq!(m.speakers[&1].queue.len(), MAX_QUEUED * FRAME_SIZE);
    }

    #[test]
    fn gains_fade_with_distance_and_pan_to_the_side() {
        let here = [0.0, 64.0, 0.0];
        let near = spatial_gains(here, 0.0, [0.0, 64.0, 1.0], 48.0);
        assert!((near[0] - near[1]).abs() < 1e-3, "straight ahead is centred: {near:?}");
        let far = spatial_gains(here, 0.0, [0.0, 64.0, 47.0], 48.0);
        assert!(far[0] < near[0] * 0.1);
        assert_eq!(spatial_gains(here, 0.0, [0.0, 64.0, 60.0], 48.0), [0.0, 0.0]);
        // Facing +Z, -X is on the right.
        let right = spatial_gains(here, 0.0, [-5.0, 64.0, 0.0], 48.0);
        assert!(right[1] > right[0] * 2.0, "{right:?}");
        // Turned to face -X (yaw 90), the same source is straight ahead.
        let ahead = spatial_gains(here, 90.0, [-5.0, 64.0, 0.0], 48.0);
        assert!((ahead[0] - ahead[1]).abs() < 1e-3, "{ahead:?}");
    }

    #[test]
    fn level_of_silence_and_full_scale() {
        assert_eq!(level_db(&[0; 960]), -127.0);
        let loud: Vec<i16> = (0..960).map(|i| if i % 2 == 0 { 32767 } else { -32767 }).collect();
        assert!(level_db(&loud) > -0.1);
    }
}
