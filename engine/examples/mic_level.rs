//! Lists the sound devices and prints the default microphone's level for a
//! few seconds, raw and after noise suppression.
//!
//!   cargo run --example mic_level [SECONDS]

use horizon_voice_engine::audio::{self, Capture};
use horizon_voice_engine::denoise::{Denoiser, Strength};

fn main() {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    println!("microphones: {:?}", audio::input_device_names());
    println!("speakers:    {:?}", audio::output_device_names());
    let capture = match Capture::open(None) {
        Ok(c) => c,
        Err(e) => {
            println!("microphone failed: {e}");
            return;
        }
    };
    println!("recording from {:?}", capture.device);
    let mut denoiser = Denoiser::new(Strength::Medium);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let (mut frames, mut zero, mut peak_raw, mut sum_raw, mut sum_clean) = (0u32, 0u32, -127f32, 0f32, 0f32);
    while std::time::Instant::now() < end {
        for mut f in capture.frames() {
            frames += 1;
            if f.iter().all(|&s| s == 0) {
                zero += 1;
            }
            let raw = audio::level_db(&f);
            denoiser.process(&mut f);
            let clean = audio::level_db(&f);
            peak_raw = peak_raw.max(raw);
            sum_raw += raw;
            sum_clean += clean;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if frames == 0 {
        println!("no audio arrived");
        return;
    }
    println!(
        "{frames} frames ({zero} all zero): raw avg {:.1} dB, peak {:.1} dB; after noise suppression avg {:.1} dB",
        sum_raw / frames as f32,
        peak_raw,
        sum_clean / frames as f32
    );
}
