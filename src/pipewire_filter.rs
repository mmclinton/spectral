use crate::audio_processor_linux::AudioProcessor;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u32 = 1;
const INPUT_CHANNELS: u32 = 2;
const BUFFER_SIZE: usize = 480; // 10ms buffer at 48kHz - MUST match RNNoise FRAME_SIZE

pub struct AudioPipeline {
    record_process: Option<Child>,
    play_process: Option<Child>,
    processing_thread: Option<thread::JoinHandle<()>>,
}

impl AudioPipeline {
    pub fn start(
        processor: Arc<AudioProcessor>,
        source_node: &str,
        sink_node: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        println!("═══════════════════════════════════════════════════════════");
        println!("[Audio Pipeline] Starting noise reduction processing");
        println!("[Audio Pipeline] Source: {}", source_node);
        println!("[Audio Pipeline] Sink: {}", sink_node);
        println!(
            "[Audio Pipeline] Input: {}Hz, {} channels (stereo → mono conversion)",
            SAMPLE_RATE, INPUT_CHANNELS
        );
        println!(
            "[Audio Pipeline] Output: {}Hz, {} channel (mono)",
            SAMPLE_RATE, CHANNELS
        );
        println!("═══════════════════════════════════════════════════════════");

        // Start pw-record for recording (capture from physical mic)
        // Use "--raw" flag for raw PCM mode, "-" for stdout
        let mut record_process = Command::new("pw-record")
            .args([
                "--target",
                source_node,
                "--rate",
                &SAMPLE_RATE.to_string(),
                "--channels",
                &INPUT_CHANNELS.to_string(),
                "--format",
                "f32",
                "--latency=10ms",
                "--volume=1.0",
                "--raw", // Critical: enable raw PCM mode
                "-",     // Output to stdout
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let record_stdout = record_process
            .stdout
            .take()
            .ok_or("Failed to capture pw-record stdout")?;

        // Start pw-play for playback (play to virtual sink)
        // Use "--raw" flag for raw PCM mode, "-" for stdin
        let mut play_process = Command::new("pw-play")
            .args([
                "--target",
                sink_node,
                "--rate",
                &SAMPLE_RATE.to_string(),
                "--channels",
                &CHANNELS.to_string(),
                "--format",
                "f32",
                "--latency=10ms",
                "--volume=1.0",
                "--raw", // Critical: enable raw PCM mode
                "-",     // Read from stdin
            ])
            .stdin(Stdio::piped())
            .spawn()?;

        let play_stdin = play_process
            .stdin
            .take()
            .ok_or("Failed to capture pw-play stdin")?;

        println!("[Audio Pipeline] ✓ pw-record and pw-play processes started");

        let processing_thread = thread::spawn(move || {
            let mut reader = std::io::BufReader::new(record_stdout);
            let mut writer = std::io::BufWriter::new(play_stdin);

            // Input: stereo float32le
            let input_frame_size = BUFFER_SIZE * INPUT_CHANNELS as usize;
            let mut input_buffer = vec![0u8; input_frame_size * 4]; // f32 = 4 bytes
            let mut stereo_samples = vec![0f32; input_frame_size];

            // Output: mono float32le
            let mut mono_samples = vec![0f32; BUFFER_SIZE];
            let mut processed_samples = vec![0f32; BUFFER_SIZE];

            // Simple one-pole lowpass filter state for smoothing RNNoise artifacts
            // Helps reduce the static artifacts from neural processing
            let mut prev_sample = 0.0f32;
            let smoothing_alpha = 0.15; // Higher = more smoothing (0.0 = none, 1.0 = full)

            println!("[Audio Pipeline] ✓ Processing thread started - RNNoise active");
            println!("[Audio Pipeline] Converting stereo → mono, applying noise reduction");
            println!("═══════════════════════════════════════════════════════════");

            loop {
                match reader.read_exact(&mut input_buffer) {
                    Ok(_) => {
                        for (i, chunk) in input_buffer.chunks_exact(4).enumerate() {
                            stereo_samples[i] =
                                f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                        }

                        // Convert stereo to mono by averaging L and R channels
                        // Also convert from float32 PCM range [-1.0, 1.0] to i16 range [-32768.0, 32767.0]
                        // RNNoise expects samples in i16 range as f32 values
                        for i in 0..BUFFER_SIZE {
                            let left = stereo_samples[i * 2];
                            let right = stereo_samples[i * 2 + 1];
                            let mono = (left + right) / 2.0;
                            // Scale from [-1.0, 1.0] to [-32768.0, 32767.0]
                            mono_samples[i] = mono * 32768.0;
                        }

                        // Process through RNNoise (input and output are in i16 range as f32)
                        processor.process_audio_buffer(&mono_samples, &mut processed_samples);

                        // Convert f32 samples back to bytes
                        // Scale back from i16 range [-32768.0, 32767.0] to float32 PCM [-1.0, 1.0]
                        // Apply gentle smoothing filter to reduce potential RNNoise musical noise artifacts
                        let mut byte_output = Vec::with_capacity(BUFFER_SIZE * 4);
                        for &sample in &processed_samples {
                            // Simple one-pole lowpass: y[n] = (1-α)·x[n] + α·y[n-1]
                            // This reduces high-frequency artifacts while preserving speech
                            let smoothed =
                                (1.0 - smoothing_alpha) * sample + smoothing_alpha * prev_sample;
                            prev_sample = smoothed;

                            let normalized = smoothed / 32768.0;
                            byte_output.extend_from_slice(&normalized.to_le_bytes());
                        }

                        if writer.write_all(&byte_output).is_err() {
                            eprintln!("[Audio Pipeline] ✗ Failed to write to pw-play - stopping");
                            break;
                        }

                        let _ = writer.flush();
                    }
                    Err(e) => {
                        eprintln!("[Audio Pipeline] ✗ Read error: {} - stopping", e);
                        break;
                    }
                }
            }

            println!("[Audio Pipeline] Processing thread terminated");
        });

        Ok(Self {
            record_process: Some(record_process),
            play_process: Some(play_process),
            processing_thread: Some(processing_thread),
        })
    }

    pub fn stop(&mut self) {
        println!("═══════════════════════════════════════════════════════════");
        println!("[Audio Pipeline] Stopping noise reduction processing...");

        if let Some(mut process) = self.record_process.take() {
            if let Err(e) = process.kill() {
                eprintln!("⚠ Failed to kill pw-record: {}", e);
            }
            match process.wait() {
                Ok(status) => println!("[Audio Pipeline] ✓ pw-record exited: {}", status),
                Err(e) => eprintln!("⚠ Failed to wait for pw-record: {}", e),
            }
        }

        if let Some(mut process) = self.play_process.take() {
            if let Err(e) = process.kill() {
                eprintln!("⚠ Failed to kill pw-play: {}", e);
            }
            match process.wait() {
                Ok(status) => println!("[Audio Pipeline] ✓ pw-play exited: {}", status),
                Err(e) => eprintln!("⚠ Failed to wait for pw-play: {}", e),
            }
        }

        if let Some(handle) = self.processing_thread.take() {
            match handle.join() {
                Ok(_) => println!("[Audio Pipeline] ✓ Processing thread stopped"),
                Err(_) => eprintln!("⚠ Processing thread panicked"),
            }
        }

        println!("═══════════════════════════════════════════════════════════");
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.stop();
    }
}
