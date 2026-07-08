//! Throwaway P0-4 spike (deleted at the end of the series): a standalone
//! `Audio/Source/Virtual` node playing a 440 Hz tone, proving on this machine
//! that an output-direction stream with that media class appears as an
//! input-only device before the rearchitecture is built on the assumption.
//!
//! Run with `cargo run --example virtual_source_spike`, then check that
//! GNOME Settings / pavucontrol list "Spectral-Spike" as an input only (no
//! new output device, no monitor), that `pw-record --target Spectral-Spike`
//! captures the tone, and that the node vanishes on Ctrl-C. The console
//! prints stream-state transitions; note where the unlinked stream settles.

use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
use pipewire::spa::pod::Pod;
use pipewire::spa::utils::Direction;
use pipewire::{
    context::ContextRc,
    main_loop::MainLoopRc,
    properties::properties,
    stream::{StreamBox, StreamFlags},
};

const RATE: u32 = 48000;
const FREQUENCY: f64 = 440.0;
const VOLUME: f32 = 0.6;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pipewire::init();
    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CATEGORY => "Playback",
        *pipewire::keys::MEDIA_CLASS => "Audio/Source/Virtual",
        *pipewire::keys::NODE_NAME => "Spectral-Spike",
        *pipewire::keys::NODE_DESCRIPTION => "Spectral-Spike",
        *pipewire::keys::NODE_VIRTUAL => "true",
        *pipewire::keys::NODE_ALWAYS_PROCESS => "true",
        *pipewire::keys::NODE_LATENCY => "480/48000",
        *pipewire::keys::AUDIO_CHANNELS => "1",
        "audio.position" => "[ MONO ]",
    };

    let stream = StreamBox::new(&core, "spectral-spike-source", props)?;

    let _listener = stream
        .add_local_listener_with_user_data(0.0f64)
        .state_changed(|_stream, _phase, old, new| {
            println!("stream state: {old:?} -> {new:?}");
        })
        .process(|stream, phase| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let requested = buffer.requested() as usize;
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let data = &mut datas[0];
                let samples = if let Some(slice) = data.data() {
                    let max_samples = slice.len() / 4;
                    let samples = if requested > 0 {
                        requested.min(max_samples)
                    } else {
                        max_samples
                    };
                    for i in 0..samples {
                        *phase = (*phase + std::f64::consts::TAU * FREQUENCY / f64::from(RATE))
                            % std::f64::consts::TAU;
                        let value = phase.sin() as f32 * VOLUME;
                        slice[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
                    }
                    samples
                } else {
                    0
                };
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = 4;
                *chunk.size_mut() = (samples * 4) as u32;
            }
        })
        .register()?;

    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(RATE);
    audio_info.set_channels(1);
    let mut position = [0; MAX_CHANNELS];
    position[0] = pipewire::spa::sys::SPA_AUDIO_CHANNEL_MONO;
    audio_info.set_position(position);

    let values: Vec<u8> = pipewire::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pipewire::spa::pod::Value::Object(pipewire::spa::pod::Object {
            type_: pipewire::spa::sys::SPA_TYPE_OBJECT_Format,
            id: pipewire::spa::param::ParamType::EnumFormat.as_raw(),
            properties: audio_info.into(),
        }),
    )
    .map_err(|e| format!("failed to serialize the audio format: {e:?}"))?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).ok_or("invalid format pod")?];

    stream.connect(
        Direction::Output,
        None,
        StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    println!("virtual source \"Spectral-Spike\" is up; Ctrl-C to remove it");
    mainloop.run();
    Ok(())
}
