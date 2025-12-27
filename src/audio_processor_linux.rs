use crate::pipewire_filter::AudioPipeline;
use crate::pulse_module::PulseContext;
use crate::state::AppState;
use nnnoiseless::DenoiseState;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
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
        let threshold = *self
            .state
            .threshold_val
            .lock()
            .expect("threshold_val mutex poisoned in audio processing")
            as f32
            / 100.0;

        let mut buffer = self
            .input_buffer
            .lock()
            .expect("input_buffer mutex poisoned in audio processing");
        buffer.extend_from_slice(input);

        let mut out_idx = 0;
        let mut denoiser = self
            .denoiser
            .lock()
            .expect("denoiser mutex poisoned in audio processing");
        let mut vad_state = self
            .vad_state
            .lock()
            .expect("vad_state mutex poisoned in audio processing");

        // Process in FRAME_SIZE chunks (480 samples = 10ms at 48kHz)
        while buffer.len() >= FRAME_SIZE {
            let frame: Vec<f32> = buffer.drain(..FRAME_SIZE).collect();

            let mut denoised_frame = vec![0.0f32; FRAME_SIZE];

            // Apply RNNoise - processes audio in i16 range [-32768.0, 32767.0]
            // Returns VAD probability (0.0-1.0) indicating voice presence confidence
            let vad_probability = denoiser.process_frame(&mut denoised_frame[..], &frame[..]);

            static FRAME_COUNTER: AtomicU32 = AtomicU32::new(0);
            let count = FRAME_COUNTER.fetch_add(1, Ordering::Relaxed);
            if count % 100 == 0 {
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
                // Release: 0.96 = ~250ms ramp down (keeps tail of words/sentences)
                let attack_coef = 0.85; // Faster attack to catch word beginning
                let release_coef = 0.96; // Faster release than previous (0.98) - closes gate quicker after voice stops

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

        // Create PulseAudio context
        let mut pulse_ctx = PulseContext::new("spectral")?;

        // Load null sink module
        let sink_args = format!(
            "sink_name={} sink_properties=device.description=Spectral-DoNotUse rate=48000 channels=1 format=float32le",
            sink_name
        );

        let sink_module_id = match pulse_ctx.load_module("module-null-sink", &sink_args) {
            Ok(id) => {
                println!("[PulseAudio] Loaded module-null-sink: module ID {}", id);
                Some(id)
            }
            Err(e) => {
                return Err(format!("Failed to create null sink: {}", e).into());
            }
        };

        let monitor_name = format!("{}.monitor", sink_name);
        let source_properties = format!(
            "device.description={} node.description={} device.icon_name=audio-input-microphone",
            display_name, display_name
        );

        println!(
            "[Debug] Creating remap source with properties: {}",
            source_properties
        );

        // Load remap source module
        let remap_args = format!(
            "source_name={} master={} source_properties={} channels=1",
            source_name, monitor_name, source_properties
        );

        let remap_module_id = match pulse_ctx.load_module("module-remap-source", &remap_args) {
            Ok(id) => {
                println!("[PulseAudio] Loaded module-remap-source: module ID {}", id);
                Some(id)
            }
            Err(e) => {
                // Cleanup sink module on failure
                if let Some(sink_id) = sink_module_id {
                    let _ = pulse_ctx.unload_module(sink_id);
                }
                return Err(format!("Failed to create remapped source: {}", e).into());
            }
        };

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

        // Give PipeWire a moment to register the new node
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Find the node ID for the virtual mic we just created
        // wpctl requires numeric ID, not name
        match Self::find_node_id_by_name(&source_name) {
            Ok(id) => {
                println!("[System] Found {} with node ID: {}", source_name, id);

                let set_default_output = Command::new("wpctl")
                    .args(["set-default", &id.to_string()])
                    .output();

                match set_default_output {
                    Ok(output) => {
                        // wpctl may write to stderr even on success, check actual result
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stdout = String::from_utf8_lossy(&output.stdout);

                        if output.status.success() {
                            // Success - wpctl returns 0 even if it writes warnings to stderr
                            if !stderr.is_empty() && stderr.contains("not found") {
                                // Benign warning, but actually succeeded
                                println!(
                                    "[System] ✓ Set {} (ID {}) as default input source",
                                    virtual_name, id
                                );
                                println!(
                                    "[System] ℹ Note: wpctl reported warnings but operation succeeded"
                                );
                            } else {
                                println!(
                                    "[System] ✓ Set {} (ID {}) as default input source",
                                    virtual_name, id
                                );
                            }
                        } else {
                            // Actual failure
                            eprintln!(
                                "[System] ⚠ Failed to set default source (exit code {})",
                                output.status
                            );
                            if !stderr.is_empty() {
                                eprintln!("[System]   Error: {}", stderr.trim());
                            }
                            if !stdout.is_empty() {
                                eprintln!("[System]   Output: {}", stdout.trim());
                            }
                            eprintln!(
                                "[System] ℹ Virtual mic '{}' created but may need manual selection",
                                virtual_name
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!("[System] Failed to execute wpctl set-default: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("[System] Could not find node ID for {}: {}", source_name, e);
                eprintln!(
                    "[System] ℹ Virtual mic '{}' created but not set as default - select manually",
                    virtual_name
                );
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
            *state
                .threshold_val
                .lock()
                .expect("threshold_val mutex poisoned during mic creation")
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

    fn find_node_id_by_name(name: &str) -> Result<u32, Box<dyn std::error::Error>> {
        let output = Command::new("wpctl").args(["status"]).output()?;

        if !output.status.success() {
            return Err("Failed to run wpctl status".into());
        }

        let output_str = String::from_utf8_lossy(&output.stdout);

        // Virtual mics created via PulseAudio appear in the Filters section as [Audio/Source]
        // Look for the Filters section first, then parse node IDs from there
        let mut in_audio_section = false;
        let mut in_filters_section = false;

        for line in output_str.lines() {
            let trimmed = line.trim();

            // Track which section we're in
            if line.starts_with("Audio") {
                in_audio_section = true;
                in_filters_section = false;
                continue;
            } else if line.starts_with("Video") || line.starts_with("Settings") {
                in_audio_section = false;
                in_filters_section = false;
                continue;
            }

            // Check for subsection markers within Audio section
            if in_audio_section {
                if trimmed.starts_with("├─ Filters:") || trimmed.starts_with("└─ Filters:")
                {
                    in_filters_section = true;
                    continue;
                } else if trimmed.starts_with("├─") || trimmed.starts_with("└─") {
                    // Different subsection (Sinks, Sources, Streams, etc.)
                    in_filters_section = false;
                    continue;
                }
            }

            // Look for our node in the Filters section
            // Format: "  *  114. Spectral-Mic                                                 [Audio/Source]"
            if in_filters_section && line.contains(name) && line.contains("[Audio/Source]") {
                // Extract the node ID (number before the dot)
                let stripped = trimmed.trim_start_matches('*').trim();
                if let Some(dot_pos) = stripped.find('.') {
                    let id_str = stripped[..dot_pos].trim();
                    if let Ok(id) = id_str.parse::<u32>() {
                        return Ok(id);
                    }
                }
            }
        }

        Err(format!("Could not find node ID for '{}' in Filters section", name).into())
    }

    pub fn stop(mut self) {
        println!();
        println!("═══════════════════════════════════════════════════════════");
        println!("Stopping virtual microphone...");

        if let Some(mut pipeline) = self.audio_pipeline.take() {
            pipeline.stop();
        }

        // Create a new PulseContext just for cleanup
        if let Ok(mut ctx) = PulseContext::new("spectral-cleanup") {
            if let Some(id) = self.remap_module_id.take() {
                match ctx.unload_module(id) {
                    Ok(_) => println!("  ✓ Unloaded remap module {}", id),
                    Err(e) => eprintln!("  ✗ Failed to unload remap module {}: {}", id, e),
                }
            }
            if let Some(id) = self.sink_module_id.take() {
                match ctx.unload_module(id) {
                    Ok(_) => println!("  ✓ Unloaded sink module {}", id),
                    Err(e) => eprintln!("  ✗ Failed to unload sink module {}: {}", id, e),
                }
            }
        } else {
            eprintln!("  ✗ Failed to create PulseContext for cleanup");
        }

        println!("Virtual microphone stopped");
        println!("═══════════════════════════════════════════════════════════");
        println!();
    }
}

impl Drop for VirtualMic {
    fn drop(&mut self) {
        // Only clean up if resources haven't been released
        // stop() takes ownership and clears these fields
        if self.audio_pipeline.is_some()
            || self.sink_module_id.is_some()
            || self.remap_module_id.is_some()
        {
            eprintln!("⚠ VirtualMic dropped without explicit stop(), cleaning up...");

            if let Some(mut pipeline) = self.audio_pipeline.take() {
                pipeline.stop();
            }

            if let Ok(mut ctx) = PulseContext::new("spectral-cleanup") {
                if let Some(id) = self.remap_module_id.take() {
                    let _ = ctx.unload_module(id);
                }
                if let Some(id) = self.sink_module_id.take() {
                    let _ = ctx.unload_module(id);
                }
            }
        }
    }
}
