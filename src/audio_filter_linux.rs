use crate::state::AppState;
use nnnoiseless::DenoiseState;
use pipewire as pw;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

const FRAME_SIZE: usize = 480; // nnnoiseless works with 480 samples at 48kHz (10ms frames)

pub struct NoiseReductionFilter {
    state: Arc<AppState>,
    denoiser: RefCell<Box<DenoiseState<'static>>>,
    input_buffer: RefCell<Vec<f32>>,
}

impl NoiseReductionFilter {
    pub fn new(state: Arc<AppState>) -> Self {
        println!(
            "[NoiseReductionFilter] Initializing with threshold: {}%",
            *state.threshold_val.lock().unwrap()
        );
        Self {
            state,
            denoiser: RefCell::new(DenoiseState::new()),
            input_buffer: RefCell::new(Vec::with_capacity(FRAME_SIZE * 2)),
        }
    }

    pub fn process(&self, input: &[f32], output: &mut [f32]) {
        let threshold = *self.state.threshold_val.lock().unwrap() as f32 / 100.0;

        let mut buffer = self.input_buffer.borrow_mut();
        buffer.extend_from_slice(input);

        let mut out_idx = 0;
        let mut denoiser = self.denoiser.borrow_mut();

        while buffer.len() >= FRAME_SIZE {
            let frame: Vec<f32> = buffer.drain(..FRAME_SIZE).collect();
            let mut denoised_frame = vec![0.0f32; FRAME_SIZE];

            denoiser.process_frame(&mut denoised_frame[..], &frame[..]);

            for &sample in denoised_frame.iter() {
                if out_idx < output.len() {
                    let abs_sample = sample.abs();
                    output[out_idx] = if abs_sample < threshold * 0.05 {
                        0.0
                    } else {
                        sample
                    };
                    out_idx += 1;
                }
            }
        }

        // Fill remaining output with zeros if input was shorter than expected
        while out_idx < output.len() {
            output[out_idx] = 0.0;
            out_idx += 1;
        }
    }
}

pub fn create_filter_stream(
    source_node_name: &str,
    sink_node_name: &str,
    state: Arc<AppState>,
) -> Result<pw::thread_loop::ThreadLoopRc, Box<dyn std::error::Error>> {
    println!("[PipeWire Filter] Creating filter stream:");
    println!("  Source: {}", source_node_name);
    println!("  Sink: {}", sink_node_name);

    let thread_loop = pw::thread_loop::ThreadLoop::new(None)?;
    let loop_clone = thread_loop.clone();

    thread_loop.lock();

    let context = pw::context::Context::new(&thread_loop)?;
    let core = context.connect(None)?;

    let filter = Rc::new(NoiseReductionFilter::new(state));
    let filter_clone = filter.clone();

    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Filter",
        *pw::keys::MEDIA_ROLE => "DSP",
        *pw::keys::NODE_NAME => "spectral_noise_filter",
        *pw::keys::NODE_DESCRIPTION => "Spectral Noise Reduction Filter",
    };

    let stream = pw::stream::Stream::new(&core, "spectral-filter", props)?;

    let stream_rc = Rc::new(stream);
    let stream_clone = stream_rc.clone();

    let _listener = stream_rc
        .add_local_listener::<pw::stream::StreamListener>()
        .process(move |stream| match stream.dequeue_buffer() {
            None => return,
            Some(mut buffer) => {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }

                let data = &mut datas[0];
                let chunk = data.chunk();
                let size = chunk.size() as usize;

                if size == 0 {
                    return;
                }

                unsafe {
                    let ptr = data.data();
                    if ptr.is_null() {
                        return;
                    }

                    let samples = size / std::mem::size_of::<f32>();
                    let input_slice = std::slice::from_raw_parts(ptr as *const f32, samples);
                    let output_slice = std::slice::from_raw_parts_mut(ptr as *mut f32, samples);

                    filter_clone.process(input_slice, output_slice);
                }
            }
        })
        .register()?;

    let mut audio_info = pw::spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(pw::spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(48000);
    audio_info.set_channels(1);

    let obj = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: vec![
            pw::spa::pod::Property {
                key: pw::spa::param::format::FormatProperties::MediaType.as_raw(),
                flags: pw::spa::pod::PropertyFlags::empty(),
                value: pw::spa::pod::Value::Id(pw::spa::param::format::MediaType::Audio.as_raw()),
            },
            pw::spa::pod::Property {
                key: pw::spa::param::format::FormatProperties::MediaSubtype.as_raw(),
                flags: pw::spa::pod::PropertyFlags::empty(),
                value: pw::spa::pod::Value::Id(pw::spa::param::format::MediaSubtype::Raw.as_raw()),
            },
            pw::spa::pod::Property {
                key: pw::spa::param::format::FormatProperties::AudioFormat.as_raw(),
                flags: pw::spa::pod::PropertyFlags::empty(),
                value: pw::spa::pod::Value::Id(pw::spa::param::audio::AudioFormat::F32LE.as_raw()),
            },
            pw::spa::pod::Property {
                key: pw::spa::param::format::FormatProperties::AudioRate.as_raw(),
                flags: pw::spa::pod::PropertyFlags::empty(),
                value: pw::spa::pod::Value::Int(48000),
            },
            pw::spa::pod::Property {
                key: pw::spa::param::format::FormatProperties::AudioChannels.as_raw(),
                flags: pw::spa::pod::PropertyFlags::empty(),
                value: pw::spa::pod::Value::Int(1),
            },
        ],
    };

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .unwrap()
    .0
    .into_inner();

    let mut params = [values.as_ptr() as *const pw::spa::pod::Pod];

    stream_clone.connect(
        pw::spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    thread_loop.unlock();

    thread_loop.start();

    println!("[PipeWire Filter] Stream created and started successfully");

    Ok(loop_clone)
}
