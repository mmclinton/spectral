use crate::pipewire_filter::AudioPipeline;
use crate::state::AppState;
use nnnoiseless::DenoiseState;
use std::process::Command;
use std::sync::{Arc, Mutex};

// Audio processing constants for nnnoiseless RNNoise implementation
const FRAME_SIZE: usize = 480; // RNNoise processes 10ms frames at 48kHz (480 samples)

// IMPORTANT: RNNoise expects samples in i16 range [-32768.0, 32767.0] as f32 values
// NOT the typical float32 PCM range of [-1.0, 1.0]
// Conversion is handled in pipewire_filter.rs before/after RNNoise processing

pub struct AudioProcessor {
    state: Arc<AppState>,
    denoiser: Mutex<Box<DenoiseState<'static>>>,
    input_buffer: Mutex<Vec<f32>>,
    vad_state: Mutex<f32>,
}

impl AudioProcessor {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            denoiser: Mutex::new(DenoiseState::new()),
            input_buffer: Mutex::new(Vec::with_capacity(FRAME_SIZE * 2)),
            vad_state: Mutex::new(0.0), // Start with no voice activity
        }
    }

    /// Called from PipeWire stream callbacks for real-time audio processing
    pub fn process_audio_buffer(&self, input: &[f32], output: &mut [f32]) {
        let threshold = *self.state.threshold_val.lock().unwrap() as f32 / 100.0;

        let mut buffer = self.input_buffer.lock().unwrap();
        buffer.extend_from_slice(input);

        let mut out_idx = 0;
        let mut denoiser = self.denoiser.lock().unwrap();
        let mut vad_state = self.vad_state.lock().unwrap();

        // Process in FRAME_SIZE chunks (480 samples = 10ms at 48kHz)
        while buffer.len() >= FRAME_SIZE {
            let frame: Vec<f32> = buffer.drain(..FRAME_SIZE).collect();

            let mut denoised_frame = vec![0.0f32; FRAME_SIZE];

            // Apply RNNoise - processes audio in i16 range [-32768.0, 32767.0]
            // Returns VAD probability (0.0-1.0) indicating voice presence confidence
            let vad_probability = denoiser.process_frame(&mut denoised_frame[..], &frame[..]);

            static mut FRAME_COUNTER: u32 = 0;
            unsafe {
                FRAME_COUNTER += 1;
                if FRAME_COUNTER.is_multiple_of(100) {
                    let input_rms =
                        (frame.iter().map(|&s| s * s).sum::<f32>() / FRAME_SIZE as f32).sqrt();
                    println!(
                        "[RNNoise] VAD: {:.3}, Gate: {:.3}, Threshold: {:.0}%, Input RMS: {:.1}",
                        vad_probability,
                        *vad_state,
                        threshold * 100.0,
                        input_rms
                    );
                }
            }

            if threshold >= 0.5 {
                // Map threshold 50-100% to VAD cutoff 0.5-0.95
                let vad_threshold = (threshold - 0.5) * 0.9 + 0.5;

                // Determine gate state from VAD probability
                let target_gate = if vad_probability > vad_threshold {
                    1.0
                } else {
                    0.0
                };

                // Smooth gate transitions to prevent clicks and word cutoff
                // These coefficients create fade-in/fade-out instead of hard cuts
                // Attack: 0.85 = ~75ms ramp up (fast enough to catch word start)
                // Release: 0.98 = ~500ms ramp down (keeps tail of words/sentences)
                let attack_coef = 0.85; // Faster attack to catch word beginning
                let release_coef = 0.98; // Slower release to avoid cutting word end

                if target_gate > 0.5 {
                    // Voice detected - ramp up gate state
                    *vad_state = attack_coef * (*vad_state) + (1.0 - attack_coef);
                } else {
                    // No voice - ramp down gate state
                    *vad_state *= release_coef;
                }

                // Apply smooth gain instead of binary gate
                // This should create a more natural fade-in/fade-out transitions
                // vad_state ranges from 0.0 (silent) to 1.0 (full volume)
                let gain = *vad_state;

                // Apply the smooth gain to create gradual transitions
                for &sample in denoised_frame.iter() {
                    if out_idx < output.len() {
                        output[out_idx] = sample * gain;
                        out_idx += 1;
                    }
                }
            } else {
                // Low threshold: passthrough denoised audio without gating
                for &sample in denoised_frame.iter() {
                    if out_idx < output.len() {
                        output[out_idx] = sample;
                        out_idx += 1;
                    }
                }
            }
        }

        while out_idx < output.len() {
            output[out_idx] = 0.0;
            out_idx += 1;
        }
    }
}

pub struct VirtualMic {
    _source_name: String,
    sink_module_id: Option<u32>,
    remap_module_id: Option<u32>,
    audio_pipeline: Option<AudioPipeline>,
    _processor: Arc<AudioProcessor>,
}

impl VirtualMic {
    pub fn create(
        state: Arc<AppState>,
        source_device_id: u32,
        virtual_name: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let processor = Arc::new(AudioProcessor::new(state.clone()));

        let source_name = virtual_name.replace(" ", "_");
        let sink_name = format!("{}_sink", source_name);
        let display_name = virtual_name.replace(" ", "-");

        let output = Command::new("pactl")
            .args([
                "load-module",
                "module-null-sink",
                &format!("sink_name={}", sink_name),
                "sink_properties=device.description=Spectral-DoNotUse",
                "rate=48000",
                "channels=1",
                "format=float32le",
            ])
            .output()?;

        if !output.status.success() {
            return Err(format!(
                "Failed to create null sink: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        let sink_module_id = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .ok();

        let monitor_name = format!("{}.monitor", sink_name);
        let source_properties = format!(
            "device.description={} node.description={} device.icon_name=audio-input-microphone",
            display_name, display_name
        );

        println!(
            "[Debug] Creating remap source with properties: {}",
            source_properties
        );

        let output = Command::new("pactl")
            .args([
                "load-module",
                "module-remap-source",
                &format!("source_name={}", source_name),
                &format!("master={}", monitor_name),
                &format!("source_properties={}", source_properties),
                "channels=1",
            ])
            .output()?;

        if !output.status.success() {
            if let Some(id) = sink_module_id {
                let _ = Command::new("pactl")
                    .args(["unload-module", &id.to_string()])
                    .output();
            }
            return Err(format!(
                "Failed to create remapped source: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        let remap_module_id = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .ok();

        println!("═══════════════════════════════════════════════════════════");
        println!(
            "✓ Virtual microphone '{}' created successfully",
            virtual_name
        );
        println!("  - Sink module ID: {:?}", sink_module_id);
        println!("  - Remap module ID: {:?}", remap_module_id);
        println!("  - PipeWire node name: {}", source_name);
        println!("═══════════════════════════════════════════════════════════");
        println!();

        let set_default_output = Command::new("pactl")
            .args(["set-default-source", &source_name])
            .output();

        match set_default_output {
            Ok(output) if output.status.success() => {
                println!("[System] ✓ Set {} as default input source", virtual_name);
            }
            Ok(output) => {
                eprintln!(
                    "[System] Failed to set default source: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(e) => {
                eprintln!("[System] Failed to execute pactl set-default-source: {}", e);
            }
        }
        println!();

        let source_node_name = Self::get_node_name(source_device_id)?;
        println!(
            "[Device] Input source (ID {}): {}",
            source_device_id, source_node_name
        );
        println!();

        // Physical Mic → parecord → RNNoise Filter → pacat → Virtual Sink → Virtual Mic
        println!(
            "[Audio Pipeline] Threshold: {}%",
            *state.threshold_val.lock().unwrap()
        );

        let audio_pipeline =
            match AudioPipeline::start(processor.clone(), &source_node_name, &sink_name) {
                Ok(pipeline) => {
                    println!();
                    println!("✓ Audio pipeline created successfully");
                    println!("[Status] 🎤 NOISE REDUCTION ACTIVE 🎤");
                    Some(pipeline)
                }
                Err(e) => {
                    eprintln!("✗ ERROR: Failed to create audio pipeline: {}", e);
                    eprintln!("  Falling back to passthrough mode");
                    None
                }
            };

        println!();

        Ok(Self {
            _source_name: source_name,
            sink_module_id,
            remap_module_id,
            audio_pipeline,
            _processor: processor,
        })
    }

    fn get_node_name(node_id: u32) -> Result<String, Box<dyn std::error::Error>> {
        let output = Command::new("wpctl")
            .args(["inspect", &node_id.to_string()])
            .output()?;

        if !output.status.success() {
            return Err(format!("Failed to inspect node {}", node_id).into());
        }

        let output_str = String::from_utf8_lossy(&output.stdout);

        for line in output_str.lines() {
            if line.contains("node.name") {
                // Line format: "    node.name = "alsa_input.usb-...""
                if let Some(name_part) = line.split('"').nth(1) {
                    return Ok(name_part.to_string());
                }
            }
        }

        Err(format!("Could not find node.name for node {}", node_id).into())
    }

    pub fn stop(mut self) {
        println!();
        println!("═══════════════════════════════════════════════════════════");
        println!("Stopping virtual microphone...");

        if let Some(mut pipeline) = self.audio_pipeline.take() {
            pipeline.stop();
        }

        if let Some(id) = self.remap_module_id {
            let _ = Command::new("pactl")
                .args(["unload-module", &id.to_string()])
                .output();
            println!("  ✓ Unloaded remap module {}", id);
        }
        if let Some(id) = self.sink_module_id {
            let _ = Command::new("pactl")
                .args(["unload-module", &id.to_string()])
                .output();
            println!("  ✓ Unloaded sink module {}", id);
        }

        println!("Virtual microphone stopped");
        println!("═══════════════════════════════════════════════════════════");
        println!();
    }
}
