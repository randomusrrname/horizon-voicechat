//! Microphone capture, playback and the Opus codec.
//!
//! cpal's streams are not `Send` on every platform, so both streams live on one worker thread; the
//! rest of the mod talks to it through shared buffers. Voice is mono, 48 kHz, 20 ms frames
//! (960 samples), as in the captures of a Paper server with Simple Voice Chat.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use rusty_opus::{Application, OpusDecoder, OpusEncoder};

pub const SAMPLE_RATE: i32 = 48_000;
/// Samples in one 20 ms mono frame.
pub const FRAME: usize = 960;
/// Largest Opus packet we will make (the server's MTU is 1275).
const MAX_PACKET: usize = 1275;
/// A speaker's audio is held back until this many frames are queued, to absorb network jitter.
const PREBUFFER_FRAMES: usize = 3;
/// More than this queued and the oldest audio is dropped, so a stall never turns into a delay.
const MAX_QUEUED_FRAMES: usize = 12;
/// Microphone audio kept while nothing consumes it.
const MAX_CAPTURE_SAMPLES: usize = SAMPLE_RATE as usize;
/// Missing frames up to this many in a row are concealed by the decoder.
const MAX_CONCEALED: i64 = 3;

fn log(message: &str) {
    eprintln!("[voicechat] {message}");
}

struct Speaker {
    decoder: OpusDecoder,
    queue: VecDeque<f32>,
    buffering: bool,
    last_sequence: Option<i64>,
    /// The speaker sent the empty frame that ends a transmission.
    ended: bool,
}

struct Shared {
    capture: Mutex<VecDeque<f32>>,
    speakers: Mutex<HashMap<[u8; 16], Speaker>>,
    stop: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Playback of other players and capture of the microphone.
pub struct Audio {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    has_input: bool,
}

impl Audio {
    /// Opens the default output and, when there is one, the default input. Fails only when there
    /// is no output; without a microphone the player can still listen.
    pub fn new() -> Result<Audio, String> {
        let shared = Arc::new(Shared {
            capture: Mutex::new(VecDeque::new()),
            speakers: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
        });
        let (ready_tx, ready_rx) = mpsc::channel::<Result<bool, String>>();
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("voicechat-audio".into())
            .spawn(move || run_worker(worker_shared, ready_tx))
            .map_err(|e| format!("could not start the audio thread: {e}"))?;
        let has_input = match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(has_input)) => has_input,
            Ok(Err(e)) => {
                let _ = worker.join();
                return Err(e);
            }
            Err(_) => {
                shared.stop.store(true, Ordering::Relaxed);
                return Err("the audio devices did not answer in 5 seconds".into());
            }
        };
        Ok(Audio { shared, worker: Some(worker), has_input })
    }

    pub fn has_input(&self) -> bool {
        self.has_input
    }

    /// The next 20 ms of microphone audio, if a whole frame has been captured.
    pub fn take_frame(&self) -> Option<Vec<f32>> {
        let mut capture = lock(&self.shared.capture);
        if capture.len() < FRAME {
            return None;
        }
        Some(capture.drain(..FRAME).collect())
    }

    /// Drops captured audio nobody will send.
    pub fn clear_capture(&self) {
        lock(&self.shared.capture).clear();
    }

    /// Queues one received voice frame of `speaker`. An empty `opus` ends their transmission.
    pub fn play(&self, speaker: [u8; 16], opus: &[u8], sequence: i64) {
        let mut speakers = lock(&self.shared.speakers);
        if !speakers.contains_key(&speaker) {
            let Ok(decoder) = OpusDecoder::new(SAMPLE_RATE, 1) else {
                log("could not create an Opus decoder");
                return;
            };
            speakers.insert(
                speaker,
                Speaker {
                    decoder,
                    queue: VecDeque::new(),
                    buffering: true,
                    last_sequence: None,
                    ended: false,
                },
            );
        }
        let Some(s) = speakers.get_mut(&speaker) else { return };
        if opus.is_empty() {
            s.ended = true;
            return;
        }
        if s.ended {
            s.ended = false;
            s.buffering = true;
            s.last_sequence = None;
        }
        let mut pcm = vec![0.0f32; FRAME];
        if let Some(last) = s.last_sequence {
            if sequence <= last {
                return; // a duplicate or a late frame
            }
            let missing = sequence - last - 1;
            if (1..=MAX_CONCEALED).contains(&missing) {
                for _ in 0..missing {
                    if let Ok(n) = s.decoder.decode(&[], FRAME, &mut pcm) {
                        s.queue.extend(pcm[..n.min(FRAME)].iter().copied());
                    }
                }
            }
        }
        match s.decoder.decode(opus, FRAME, &mut pcm) {
            Ok(n) => s.queue.extend(pcm[..n.min(FRAME)].iter().copied()),
            Err(_) => return, // a damaged frame: skip it, keep the old sequence
        }
        s.last_sequence = Some(sequence);
        let cap = MAX_QUEUED_FRAMES * FRAME;
        while s.queue.len() > cap {
            s.queue.pop_front();
        }
    }

    /// Forgets speakers that finished and have nothing left to play.
    pub fn collect_finished(&self) {
        lock(&self.shared.speakers).retain(|_, s| !(s.ended && s.queue.is_empty()));
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The microphone's Opus encoder.
pub struct Mic {
    encoder: OpusEncoder,
}

impl Mic {
    pub fn new() -> Result<Mic, String> {
        let mut encoder = OpusEncoder::new(SAMPLE_RATE, 1, Application::Voip)
            .map_err(|e| format!("could not create the Opus encoder: {e:?}"))?;
        encoder.bitrate_bps = 24_000;
        Ok(Mic { encoder })
    }

    /// Encodes one 20 ms frame (960 mono samples in -1.0..=1.0).
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, String> {
        let mut out = [0u8; MAX_PACKET];
        let n = self
            .encoder
            .encode(pcm, FRAME, &mut out)
            .map_err(|e| format!("Opus encoding failed: {e:?}"))?;
        Ok(out[..n].to_vec())
    }
}

fn run_worker(shared: Arc<Shared>, ready: mpsc::Sender<Result<bool, String>>) {
    let host = cpal::default_host();
    let output = match host.default_output_device() {
        Some(device) => build_output(&device, shared.clone()),
        None => Err("no audio output device".to_owned()),
    };
    let output = match output {
        Ok(stream) => stream,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let input = host
        .default_input_device()
        .ok_or_else(|| "no microphone found".to_owned())
        .and_then(|device| build_input(&device, shared.clone()));
    if let Err(e) = &input {
        log(&format!("no microphone: {e}"));
    }
    if let Err(e) = output.play() {
        let _ = ready.send(Err(format!("could not start playback: {e}")));
        return;
    }
    let input = input.ok().and_then(|stream| match stream.play() {
        Ok(()) => Some(stream),
        Err(e) => {
            log(&format!("could not start the microphone: {e}"));
            None
        }
    });
    let _ = ready.send(Ok(input.is_some()));
    while !shared.stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(50));
    }
    drop(input);
    drop(output);
}

/// Linear resampling of a stream that arrives in chunks, to 48 kHz.
struct Downsampler {
    step: f64,
    pos: f64,
    prev: f32,
}

impl Downsampler {
    fn new(from_rate: u32) -> Downsampler {
        Downsampler { step: from_rate as f64 / SAMPLE_RATE as f64, pos: 0.0, prev: 0.0 }
    }

    /// Feeds mono samples at the device rate; calls `out` for each sample at 48 kHz.
    fn process(&mut self, input: &[f32], mut out: impl FnMut(f32)) {
        let n = input.len();
        if n == 0 {
            return;
        }
        // Position 0 is the last sample of the previous chunk, position i + 1 is `input[i]`.
        while self.pos < n as f64 {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;
            let a = if i == 0 { self.prev } else { input[i - 1] };
            let b = input[i];
            out(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev = input[n - 1];
    }
}

/// Linear resampling of 48 kHz audio pulled one sample at a time, to the device rate.
struct Upsampler {
    step: f64,
    t: f64,
    a: f32,
    b: f32,
}

impl Upsampler {
    fn new(to_rate: u32) -> Upsampler {
        Upsampler { step: SAMPLE_RATE as f64 / to_rate as f64, t: 1.0, a: 0.0, b: 0.0 }
    }

    fn next(&mut self, pull: &mut dyn FnMut() -> f32) -> f32 {
        while self.t >= 1.0 {
            self.a = self.b;
            self.b = pull();
            self.t -= 1.0;
        }
        let sample = self.a + (self.b - self.a) * self.t as f32;
        self.t += self.step;
        sample
    }
}

/// Sums one sample of every speaker that is playing.
fn mix_sample(speakers: &mut HashMap<[u8; 16], Speaker>) -> f32 {
    let mut sum = 0.0f32;
    for s in speakers.values_mut() {
        if s.buffering {
            let ready = s.queue.len() >= PREBUFFER_FRAMES * FRAME || (s.ended && !s.queue.is_empty());
            if !ready {
                continue;
            }
            s.buffering = false;
        }
        match s.queue.pop_front() {
            Some(v) => sum += v,
            None => s.buffering = true, // ran dry: wait for more audio before playing again
        }
    }
    sum.clamp(-1.0, 1.0)
}

fn on_stream_error(e: cpal::StreamError) {
    log(&format!("audio stream error: {e}"));
}

fn build_input(device: &cpal::Device, shared: Arc<Shared>) -> Result<Stream, String> {
    let supported = device
        .default_input_config()
        .map_err(|e| format!("no input configuration: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels.max(1) as usize;
    let mut resampler = Downsampler::new(config.sample_rate.0);
    let mut mono: Vec<f32> = Vec::new();
    let mut handle = move |samples: &[f32]| {
        mono.clear();
        mono.extend(
            samples
                .chunks(channels)
                .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32),
        );
        let mut capture = lock(&shared.capture);
        resampler.process(&mono, |s| capture.push_back(s));
        while capture.len() > MAX_CAPTURE_SAMPLES {
            capture.pop_front();
        }
    };
    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| handle(data),
            on_stream_error,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let floats: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                handle(&floats)
            },
            on_stream_error,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let floats: Vec<f32> = data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect();
                handle(&floats)
            },
            on_stream_error,
            None,
        ),
        other => return Err(format!("unsupported microphone sample format {other:?}")),
    };
    stream.map_err(|e| format!("could not open the microphone: {e}"))
}

fn build_output(device: &cpal::Device, shared: Arc<Shared>) -> Result<Stream, String> {
    let supported = device
        .default_output_config()
        .map_err(|e| format!("no output configuration: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels.max(1) as usize;
    let mut resampler = Upsampler::new(config.sample_rate.0);
    // Fills `frames` interleaved device frames with the mixed voices, one value per frame.
    let mut fill = move |frames: usize, mut write: Box<dyn FnMut(usize, f32) + '_>| {
        let mut speakers = lock(&shared.speakers);
        let mut pull = || mix_sample(&mut speakers);
        for frame in 0..frames {
            let sample = resampler.next(&mut pull);
            write(frame, sample);
        }
    };
    let stream = match format {
        SampleFormat::F32 => device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                fill(
                    frames,
                    Box::new(|frame, sample| {
                        for c in 0..channels {
                            data[frame * channels + c] = sample;
                        }
                    }),
                );
            },
            on_stream_error,
            None,
        ),
        SampleFormat::I16 => device.build_output_stream(
            &config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                fill(
                    frames,
                    Box::new(|frame, sample| {
                        let v = (sample * 32767.0) as i16;
                        for c in 0..channels {
                            data[frame * channels + c] = v;
                        }
                    }),
                );
            },
            on_stream_error,
            None,
        ),
        SampleFormat::U16 => device.build_output_stream(
            &config,
            move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                fill(
                    frames,
                    Box::new(|frame, sample| {
                        let v = ((sample * 0.5 + 0.5) * 65535.0) as u16;
                        for c in 0..channels {
                            data[frame * channels + c] = v;
                        }
                    }),
                );
            },
            on_stream_error,
            None,
        ),
        other => return Err(format!("unsupported speaker sample format {other:?}")),
    };
    stream.map_err(|e| format!("could not open the speakers: {e}"))
}
