#[cfg(feature = "voice")]
mod enabled {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use anyhow::{Context, Result, anyhow, bail};
    use cpal::{
        SampleFormat, Stream, StreamConfig,
        traits::{DeviceTrait, HostTrait, StreamTrait},
    };
    use tokio::sync::mpsc;

    use crate::client::NetCommand;

    pub struct Audio {
        output_rate: u32,
        playback: Arc<Mutex<VecDeque<f32>>>,
        _output_stream: Stream,
        input_stream: Option<Stream>,
        network: mpsc::Sender<NetCommand>,
    }

    impl Audio {
        pub fn new(network: mpsc::Sender<NetCommand>) -> Result<Self> {
            let host = cpal::default_host();
            let device = host
                .default_output_device()
                .ok_or_else(|| anyhow!("no default audio output device"))?;
            let supported = device
                .default_output_config()
                .context("cannot inspect the output device")?;
            let format = supported.sample_format();
            let config: StreamConfig = supported.into();
            let output_rate = config.sample_rate.0;
            let playback = Arc::new(Mutex::new(VecDeque::new()));
            let output_stream = build_output(&device, &config, format, playback.clone())?;
            output_stream.play().context("cannot start audio output")?;
            Ok(Self {
                output_rate,
                playback,
                _output_stream: output_stream,
                input_stream: None,
                network,
            })
        }

        pub fn start_capture(&mut self) -> Result<()> {
            if self.input_stream.is_some() {
                return Ok(());
            }
            let host = cpal::default_host();
            let device = host
                .default_input_device()
                .ok_or_else(|| anyhow!("no default microphone"))?;
            let supported = device
                .default_input_config()
                .context("cannot inspect the microphone")?;
            let format = supported.sample_format();
            let config: StreamConfig = supported.into();
            let stream = build_input(&device, &config, format, self.network.clone())?;
            stream.play().context("cannot start microphone capture")?;
            self.input_stream = Some(stream);
            self.network
                .try_send(NetCommand::StartCall)
                .map_err(|_| anyhow!("network task ended"))?;
            Ok(())
        }

        pub fn stop_capture(&mut self) {
            self.input_stream = None;
            let _ = self.network.try_send(NetCommand::StopCall);
        }

        pub fn play_pcm(&self, input_rate: u32, samples: &[i16]) {
            if input_rate == 0 || samples.is_empty() {
                return;
            }
            let ratio = input_rate as f64 / self.output_rate as f64;
            let output_len = ((samples.len() as f64) / ratio).ceil() as usize;
            let mut queue = self
                .playback
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let max_buffer = self.output_rate as usize * 2;
            if queue.len() > max_buffer {
                queue.clear();
            }
            for output_index in 0..output_len {
                let position = output_index as f64 * ratio;
                let left = position.floor() as usize;
                let right = (left + 1).min(samples.len() - 1);
                let fraction = (position - left as f64) as f32;
                let a = samples[left] as f32 / i16::MAX as f32;
                let b = samples[right] as f32 / i16::MAX as f32;
                queue.push_back(a + (b - a) * fraction);
            }
        }

        pub fn is_capturing(&self) -> bool {
            self.input_stream.is_some()
        }
    }

    fn build_input(
        device: &cpal::Device,
        config: &StreamConfig,
        format: SampleFormat,
        network: mpsc::Sender<NetCommand>,
    ) -> Result<Stream> {
        let channels = config.channels as usize;
        let sample_rate = config.sample_rate.0;
        let frame_samples = (sample_rate / 50).max(1) as usize;
        let error = |error| tracing::warn!("audio input error: {error}");
        let stream = match format {
            SampleFormat::F32 => {
                let mut pending = Vec::with_capacity(frame_samples * 2);
                device.build_input_stream(
                    config,
                    move |data: &[f32], _| {
                        collect_mono(
                            data.chunks(channels).map(|frame| {
                                let sum: f32 = frame.iter().copied().sum();
                                ((sum / channels as f32).clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                            }),
                            &mut pending,
                            frame_samples,
                            sample_rate,
                            &network,
                        )
                    },
                    error,
                    None,
                )?
            }
            SampleFormat::I16 => {
                let mut pending = Vec::with_capacity(frame_samples * 2);
                device.build_input_stream(
                    config,
                    move |data: &[i16], _| {
                        collect_mono(
                            data.chunks(channels).map(|frame| {
                                let sum: i32 = frame.iter().map(|sample| *sample as i32).sum();
                                (sum / channels as i32) as i16
                            }),
                            &mut pending,
                            frame_samples,
                            sample_rate,
                            &network,
                        )
                    },
                    error,
                    None,
                )?
            }
            SampleFormat::U16 => {
                let mut pending = Vec::with_capacity(frame_samples * 2);
                device.build_input_stream(
                    config,
                    move |data: &[u16], _| {
                        collect_mono(
                            data.chunks(channels).map(|frame| {
                                let sum: u32 = frame.iter().map(|sample| *sample as u32).sum();
                                ((sum / channels as u32) as i32 - 32_768) as i16
                            }),
                            &mut pending,
                            frame_samples,
                            sample_rate,
                            &network,
                        )
                    },
                    error,
                    None,
                )?
            }
            _ => bail!("unsupported microphone sample format {format:?}"),
        };
        Ok(stream)
    }

    fn collect_mono(
        samples: impl Iterator<Item = i16>,
        pending: &mut Vec<i16>,
        frame_samples: usize,
        sample_rate: u32,
        network: &mpsc::Sender<NetCommand>,
    ) {
        pending.extend(samples);
        while pending.len() >= frame_samples {
            let remainder = pending.split_off(frame_samples);
            let frame = std::mem::replace(pending, remainder);
            let _ = network.try_send(NetCommand::VoicePcm {
                sample_rate,
                samples: frame,
            });
        }
    }

    fn build_output(
        device: &cpal::Device,
        config: &StreamConfig,
        format: SampleFormat,
        playback: Arc<Mutex<VecDeque<f32>>>,
    ) -> Result<Stream> {
        let channels = config.channels as usize;
        let error = |error| tracing::warn!("audio output error: {error}");
        let stream = match format {
            SampleFormat::F32 => device.build_output_stream(
                config,
                move |data: &mut [f32], _| fill_output(data, channels, &playback, |value| value),
                error,
                None,
            )?,
            SampleFormat::I16 => device.build_output_stream(
                config,
                move |data: &mut [i16], _| {
                    fill_output(data, channels, &playback, |value| {
                        (value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                    })
                },
                error,
                None,
            )?,
            SampleFormat::U16 => device.build_output_stream(
                config,
                move |data: &mut [u16], _| {
                    fill_output(data, channels, &playback, |value| {
                        ((value.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16
                    })
                },
                error,
                None,
            )?,
            _ => bail!("unsupported speaker sample format {format:?}"),
        };
        Ok(stream)
    }

    fn fill_output<T: Copy>(
        output: &mut [T],
        channels: usize,
        playback: &Arc<Mutex<VecDeque<f32>>>,
        convert: impl Fn(f32) -> T,
    ) {
        let mut queue = playback.lock().unwrap_or_else(|error| error.into_inner());
        for frame in output.chunks_mut(channels) {
            let sample = queue.pop_front().unwrap_or(0.0);
            for channel in frame {
                *channel = convert(sample);
            }
        }
    }
}

#[cfg(not(feature = "voice"))]
mod enabled {
    use anyhow::{Result, bail};
    use tokio::sync::mpsc;

    use crate::client::NetCommand;

    pub struct Audio;

    impl Audio {
        pub fn new(_network: mpsc::Sender<NetCommand>) -> Result<Self> {
            bail!("voice support was disabled at compile time")
        }

        pub fn start_capture(&mut self) -> Result<()> {
            bail!("voice support was disabled at compile time")
        }

        pub fn stop_capture(&mut self) {}
        pub fn play_pcm(&self, _input_rate: u32, _samples: &[i16]) {}
        pub fn is_capturing(&self) -> bool {
            false
        }
    }
}

pub use enabled::Audio;
