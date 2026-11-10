//! 按 `SOUNDBIAS` 的间隔，把音源已经算出的电平记成 PWM 采样。
//!
//! 方波、波形、噪声的倒数仍在音源里。这里只在累计周期走到采样点时读一次当前电平。
//! `0x04000088` 的低 10 位是偏置，高 2 位选择间隔：512、256、128、64 个 CPU 周期，
//! 对应 32768、65536、131072、262144 Hz。电平先加偏置，夹到 `0x000`–`0x3FF`，
//! 再按这一档右移 1 到 4 位。
//!
//! 样本进环形缓冲，那是数据面。采样率、偏置和丢样计数在控制面，不写进样本。
//! 模拟线程只生产，播放线程只取。

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};

use crate::ring::Ring;
use serde::{Deserialize, Serialize};

/// 一帧立体声。播放端拿到的就是这个，不再回头读音源寄存器。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub left: i16,
    pub right: i16,
}

/// 控制面。播放端用它知道现在的速率，不用从样本里猜。
#[derive(Debug)]
struct Control {
    hz: AtomicU32,
    bias: AtomicU16,
    produced: AtomicU64,
    overruns: AtomicU64,
    consumed: AtomicU64,
    discard: AtomicBool,
}

/// 模拟和播放之间的那条路。克隆只是多一个把手，环还是同一个。
#[derive(Clone, Debug)]
pub struct AudioLink {
    control: Arc<Control>,
    data: Arc<Ring<Sample>>,
}

/// 播放线程。窗口把它放在模拟旁边，取走样本，不占用 `advance`。
pub struct AudioPlay {
    link: AudioLink,
    stream: Option<Stream>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    taken: Arc<Mutex<Vec<Sample>>>,
}

/// 环里大约能放下最快档两帧。再多就丢新样本，模拟不等播放。
const AUDIO_CAP: usize = 8192;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Sampler {
    accum: u32,
    #[serde(default)]
    muted: bool,
    /// 高于 1 倍时每隔这么多样本留一个，喇叭仍按原来的采样率播。
    #[serde(default = "one_u8")]
    stride: u8,
    #[serde(default)]
    phase: u8,
    #[serde(skip, default = "state_audio_link")]
    link: AudioLink,
}

fn one_u8() -> u8 {
    1
}

fn state_audio_link() -> AudioLink {
    AudioLink::new(AUDIO_CAP)
}

impl AudioLink {
    fn new(capacity: usize) -> Self {
        Self {
            control: Arc::new(Control {
                hz: AtomicU32::new(sample_hz(0)),
                bias: AtomicU16::new(0),
                produced: AtomicU64::new(0),
                overruns: AtomicU64::new(0),
                consumed: AtomicU64::new(0),
                discard: AtomicBool::new(false),
            }),
            data: Arc::new(Ring::new(capacity)),
        }
    }

    /// 模拟线程写入。满了记一次丢样，不阻塞。
    fn produce(&self, sample: Sample, bias: u16) {
        self.control.hz.store(sample_hz(bias), Ordering::Relaxed);
        self.control.bias.store(bias, Ordering::Relaxed);
        if self.data.push(sample) {
            self.control.produced.fetch_add(1, Ordering::Relaxed);
        } else {
            self.control.overruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 模拟线程请求播放端丢掉积压。环仍然只有播放端在取。
    fn request_discard(&self) {
        self.control.discard.store(true, Ordering::Release);
    }

    /// 播放端丢掉积压并清掉请求。返回这次是不是真的清过。
    fn acknowledge_discard(&self) -> bool {
        if !self.control.discard.swap(false, Ordering::AcqRel) {
            return false;
        }
        while self.data.pop().is_some() {
            self.control.consumed.fetch_add(1, Ordering::Relaxed);
        }
        true
    }

    /// 播放端取走最早的一帧。
    pub fn pop(&self) -> Option<Sample> {
        let sample = self.data.pop()?;
        self.control.consumed.fetch_add(1, Ordering::Relaxed);
        Some(sample)
    }

    pub fn hz(&self) -> u32 {
        self.control.hz.load(Ordering::Relaxed)
    }

    pub fn bias(&self) -> u16 {
        self.control.bias.load(Ordering::Relaxed)
    }

    pub fn overruns(&self) -> u64 {
        self.control.overruns.load(Ordering::Relaxed)
    }

    pub fn produced(&self) -> u64 {
        self.control.produced.load(Ordering::Relaxed)
    }

    pub fn queued(&self) -> usize {
        self.data.len()
    }

    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    fn pending(&self) -> Vec<Sample> {
        self.data.pending()
    }
}

impl AudioPlay {
    /// 将模拟器采样环接到系统默认输出设备。
    pub fn spawn(link: AudioLink, volume: Arc<AtomicU8>) -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| "找不到系统默认音频输出设备".to_string())?;
        let supported = device
            .default_output_config()
            .map_err(|error| format!("读取音频输出设备配置失败：{error}"))?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let channels = config.channels;
        let output_hz = config.sample_rate.0;
        let mut state = OutputState::default();
        let stream = match format {
            SampleFormat::F32 => {
                let link = link.clone();
                let volume = Arc::clone(&volume);
                device.build_output_stream(
                    &config,
                    move |output: &mut [f32], _| {
                        fill_output(
                            output,
                            channels,
                            output_hz,
                            &link,
                            &volume,
                            &mut state,
                            |sample| f32::from(sample) / 32_768.0,
                        )
                    },
                    |error| eprintln!("音频输出流异常：{error}"),
                    None,
                )
            }
            SampleFormat::I16 => {
                let link = link.clone();
                let volume = Arc::clone(&volume);
                let mut state = OutputState::default();
                device.build_output_stream(
                    &config,
                    move |output: &mut [i16], _| {
                        fill_output(
                            output,
                            channels,
                            output_hz,
                            &link,
                            &volume,
                            &mut state,
                            |sample| sample,
                        )
                    },
                    |error| eprintln!("音频输出流异常：{error}"),
                    None,
                )
            }
            SampleFormat::U16 => {
                let link = link.clone();
                let volume = Arc::clone(&volume);
                let mut state = OutputState::default();
                device.build_output_stream(
                    &config,
                    move |output: &mut [u16], _| {
                        fill_output(
                            output,
                            channels,
                            output_hz,
                            &link,
                            &volume,
                            &mut state,
                            |sample| (i32::from(sample) + 32_768) as u16,
                        )
                    },
                    |error| eprintln!("音频输出流异常：{error}"),
                    None,
                )
            }
            _ => return Err(format!("不支持的音频采样格式：{format:?}")),
        }
        .map_err(|error| format!("创建音频输出流失败：{error}"))?;
        stream
            .play()
            .map_err(|error| format!("启动音频输出失败：{error}"))?;
        Ok(Self {
            link,
            stream: Some(stream),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            taken: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// 测试用采样收集器，不连接系统音频设备。
    pub fn collect(link: AudioLink) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let taken = Arc::new(Mutex::new(Vec::new()));
        let flag = Arc::clone(&stop);
        let heard = Arc::clone(&taken);
        let endpoint = link.clone();
        let thread = thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                if !drain(&endpoint, &heard, true) {
                    thread::park_timeout(Duration::from_millis(1));
                }
            }
            while drain(&endpoint, &heard, true) {}
        });
        Self {
            link,
            stream: None,
            stop,
            thread: Some(thread),
            taken,
        }
    }

    pub fn hz(&self) -> u32 {
        self.link.hz()
    }

    pub fn overruns(&self) -> u64 {
        self.link.overruns()
    }

    /// 停掉播放线程，返回它取走的样本。普通播放不收集，这里是空的。
    pub fn finish(mut self) -> Vec<Sample> {
        self.shutdown();
        std::mem::take(&mut *self.taken.lock().expect("播放记录还在"))
    }
}

impl Drop for AudioPlay {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl AudioPlay {
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.stream.take();
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            thread.join().ok();
        }
    }
}

#[derive(Default)]
struct OutputState {
    phase: u64,
    last: Sample,
}

impl Default for Sample {
    fn default() -> Self {
        Self { left: 0, right: 0 }
    }
}

fn fill_output<T: Copy>(
    output: &mut [T],
    channels: u16,
    output_hz: u32,
    link: &AudioLink,
    volume: &AtomicU8,
    state: &mut OutputState,
    mut convert: impl FnMut(i16) -> T,
) {
    let channels = usize::from(channels);
    if channels == 0 || output_hz == 0 {
        return;
    }
    let volume = i32::from(volume.load(Ordering::Relaxed).min(100));
    if link.acknowledge_discard() {
        state.last = Sample::default();
        state.phase = 0;
    }
    for frame in output.chunks_exact_mut(channels) {
        state.phase += u64::from(link.hz());
        while state.phase >= u64::from(output_hz) {
            if let Some(sample) = link.pop() {
                state.last = signed_sample(sample, link.bias());
            }
            state.phase -= u64::from(output_hz);
        }
        let left = (i32::from(state.last.left) * volume / 100) as i16;
        let right = (i32::from(state.last.right) * volume / 100) as i16;
        let mono = ((i32::from(left) + i32::from(right)) / 2) as i16;
        for (channel, output_sample) in frame.iter_mut().enumerate() {
            let sample = match (channels, channel) {
                (1, 0) => mono,
                (_, 0) => left,
                (_, 1) => right,
                _ => 0,
            };
            *output_sample = convert(sample);
        }
    }
}

fn signed_sample(sample: Sample, bias: u16) -> Sample {
    let shift = ((bias >> 14) & 3) + 1;
    let midpoint = i32::from(bias & 0x03FF) >> shift;
    let gain = 64i32 << (shift - 1);
    let scale = |value: i16| {
        ((i32::from(value) - midpoint) * gain).clamp(i32::from(i16::MIN), i32::from(i16::MAX))
            as i16
    };
    Sample {
        left: scale(sample.left),
        right: scale(sample.right),
    }
}

fn drain(link: &AudioLink, taken: &Mutex<Vec<Sample>>, collect: bool) -> bool {
    let _ = link.acknowledge_discard();
    let mut any = false;
    while let Some(sample) = link.pop() {
        any = true;
        if collect {
            taken.lock().expect("播放记录还在").push(sample);
        }
    }
    any
}

impl Sampler {
    pub(crate) fn new() -> Self {
        Self {
            accum: 0,
            muted: false,
            stride: 1,
            phase: 0,
            link: AudioLink::new(AUDIO_CAP),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// `speed` 是 1 时每个样本都留。更大时刚切换清一次积压，之后按倍数抽稀。
    pub(crate) fn set_output_stride(&mut self, speed: u8) {
        let stride = speed.max(1);
        if stride == self.stride.max(1) {
            return;
        }
        self.stride = stride;
        self.phase = 0;
        if stride > 1 {
            self.discard();
        }
    }

    pub(crate) fn discard(&self) {
        self.link.request_discard();
    }

    pub(crate) fn accum(&self) -> u32 {
        self.accum
    }

    pub(crate) fn advance_accum(&mut self, cycles: u32) {
        self.accum += cycles;
    }

    pub(crate) fn latch(&mut self, level: (i16, i16), bias: u16) {
        if self.muted {
            self.accum = 0;
            return;
        }
        let stride = self.stride.max(1);
        self.phase = self.phase.saturating_add(1);
        if self.phase < stride {
            self.accum = 0;
            return;
        }
        self.phase = 0;
        self.link.produce(
            Sample {
                left: pwm(level.0, bias),
                right: pwm(level.1, bias),
            },
            bias,
        );
        self.accum = 0;
    }

    /// 还没被播放线程取走的 PWM 采样。没有播放线程时，这就是从开机记到现在的全部。
    pub(crate) fn samples(&self) -> (Vec<i16>, Vec<i16>) {
        let frames = self.link.pending();
        let mut left = Vec::with_capacity(frames.len());
        let mut right = Vec::with_capacity(frames.len());
        for frame in frames {
            left.push(frame.left);
            right.push(frame.right);
        }
        (left, right)
    }

    pub(crate) fn link(&self) -> AudioLink {
        self.link.clone()
    }

    /// 换回正在播放的那条环，并让播放端丢掉积压。回退不能另开一条路。
    pub(crate) fn rebind(&mut self, link: AudioLink) {
        self.link = link;
        self.link.request_discard();
    }

    pub(crate) fn hz(&self) -> u32 {
        self.link.hz()
    }

    pub(crate) fn overruns(&self) -> u64 {
        self.link.overruns()
    }

    pub(crate) fn queue(&self) -> (usize, usize, u32) {
        (self.link.queued(), self.link.capacity(), self.link.hz())
    }
}

pub fn sample_period(bias: u16) -> u32 {
    match (bias >> 14) & 3 {
        0 => 512,
        1 => 256,
        2 => 128,
        _ => 64,
    }
}

pub fn sample_hz(bias: u16) -> u32 {
    16_777_216 / sample_period(bias)
}

/// 按还没播出去的样本，把一帧的等待拉长或缩短。
///
/// `1.0` 是积压刚好。播放跟不上、环里堆多了，就放慢模拟；快播空了，就稍快一点补上。
/// 目标大约 50ms，并夹在环容量的八分之一到一半之间。偏离最多 8%。环真的满了仍会丢掉新样本。
pub fn audio_pace(queued: usize, capacity: usize, hz: u32) -> f64 {
    if hz == 0 || capacity < 8 {
        return 1.0;
    }
    let window = (u64::from(hz) / 20) as usize;
    let target = window.clamp(capacity / 8, capacity / 2).max(1);
    let error = queued as f64 - target as f64;
    (1.0 + error / target as f64 * 0.12).clamp(0.92, 1.08)
}

pub fn pwm(level: i16, bias: u16) -> i16 {
    let shift = ((bias >> 14) & 3) + 1;
    let sum = i32::from(level) + i32::from(bias & 0x3FF);
    let clipped = sum.clamp(0, 0x3FF);
    (clipped >> shift) as i16
}

pub fn wav_bytes(left: &[i16], right: &[i16], hz: u32) -> Vec<u8> {
    let frames = left.len().min(right.len());
    let data_len = (frames * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&hz.to_le_bytes());
    out.extend_from_slice(&(hz * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for index in 0..frames {
        out.extend_from_slice(&left[index].to_le_bytes());
        out.extend_from_slice(&right[index].to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;

    const SOUND: u32 = 0x0400_0000;

    fn master(bus: &mut Bus) {
        bus.write_u16(SOUND + 0x84, 0x0080);
        bus.write_u16(SOUND + 0x80, (1 << 9) | 0x0077);
        bus.write_u16(SOUND + 0x82, 2);
    }

    #[test]
    fn audio_pace_slows_when_playback_falls_behind_and_speeds_up_when_the_queue_is_low() {
        let hz = 32_768;
        let capacity = 8192;
        let target = (hz / 20) as usize;
        assert!((audio_pace(target, capacity, hz) - 1.0).abs() < 1e-9);
        assert!(audio_pace(capacity, capacity, hz) > 1.0);
        assert!(audio_pace(0, capacity, hz) < 1.0);
        assert_eq!(audio_pace(capacity, capacity, hz), 1.08);
        assert_eq!(audio_pace(0, capacity, hz), 0.92);
        assert_eq!(audio_pace(10, capacity, 0), 1.0);
        assert_eq!(audio_pace(10, 4, hz), 1.0);
        let fast = 262_144;
        let fast_target = (capacity / 2).min((fast / 20) as usize);
        assert!((audio_pace(fast_target, capacity, fast) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn fast_forward_keeps_one_sample_from_each_speed_group() {
        let mut sampler = Sampler::new();
        sampler.set_output_stride(4);
        for level in 0..8 {
            sampler.latch((level, level), 0x200);
        }
        assert_eq!(sampler.samples().0.len(), 2);
        sampler.set_output_stride(1);
        sampler.latch((9, 9), 0x200);
        assert_eq!(sampler.samples().0.len(), 3);
    }

    #[test]
    fn a_muted_sampler_drops_pending_audio_and_produces_nothing() {
        let mut sampler = Sampler::new();
        sampler.latch((0, 0), 0x200);
        assert_eq!(sampler.samples().0.len(), 1);
        sampler.set_muted(true);
        sampler.discard();
        assert!(sampler.link().acknowledge_discard());
        sampler.latch((100, 100), 0x200);
        assert!(sampler.samples().0.is_empty());
        sampler.set_muted(false);
        sampler.latch((0, 0), 0x200);
        assert_eq!(sampler.samples().0.len(), 1);
    }

    #[test]
    fn bias_is_added_when_the_level_is_latched() {
        let mut bus = Bus::new();
        bus.write_u16(SOUND + 0x88, 0x0200);
        bus.advance(511);
        assert!(bus.audio_samples().0.is_empty());
        bus.advance(1);
        assert_eq!(bus.audio(), (0, 0));
        assert_eq!(bus.audio_samples(), (vec![256], vec![256]));
        assert_eq!(bus.sample_rate(), 32_768);
    }

    #[test]
    fn latched_samples_follow_the_square_step() {
        let mut bus = Bus::new();
        master(&mut bus);
        bus.write_u16(SOUND + 0x88, 0xC000);
        bus.write_u16(SOUND + 0x68, 0xF180);
        bus.write_u16(SOUND + 0x6C, 0x8000 | 2040);
        bus.advance(64);
        assert_eq!(bus.audio().1, 15 * 7 * 4);
        assert_eq!(bus.audio_samples(), (vec![0], vec![26]));
        bus.advance(64);
        assert_eq!(bus.audio().1, 0);
        assert_eq!(bus.audio_samples(), (vec![0, 0], vec![26, 0]));
        assert_eq!(bus.sample_rate(), 262_144);
    }

    #[test]
    fn latch_spacing_follows_soundbias() {
        let mut bus = Bus::new();
        bus.write_u16(SOUND + 0x88, 0x0200);
        bus.advance(512);
        assert_eq!(bus.audio_samples().0, &[256]);
        bus.write_u16(SOUND + 0x88, 0xC200);
        bus.advance(63);
        assert_eq!(bus.audio_samples().0.len(), 1);
        bus.advance(1);
        assert_eq!(bus.audio_samples().0, &[256, 32]);
        assert_eq!(bus.audio_hz(), 262_144);
        assert_eq!(bus.audio_overruns(), 0);
        let (left, right) = bus.audio_samples();
        let bytes = wav_bytes(&left, &right, bus.sample_rate());
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[24..28], &262_144u32.to_le_bytes());
        assert_eq!(&bytes[44..46], &256i16.to_le_bytes());
        assert_eq!(&bytes[48..50], &32i16.to_le_bytes());
    }

    #[test]
    fn playback_hears_the_latched_samples() {
        let mut alone = Bus::new();
        master(&mut alone);
        alone.write_u16(SOUND + 0x88, 0xC000);
        alone.write_u16(SOUND + 0x68, 0xF180);
        alone.write_u16(SOUND + 0x6C, 0x8000 | 2040);
        alone.advance(128);

        let mut live = Bus::new();
        master(&mut live);
        live.write_u16(SOUND + 0x88, 0xC000);
        live.write_u16(SOUND + 0x68, 0xF180);
        live.write_u16(SOUND + 0x6C, 0x8000 | 2040);
        let play = live.collect_audio();
        live.advance(128);
        let heard = play.finish();
        assert_eq!(heard.len(), 2);
        assert_eq!(heard[0], Sample { left: 0, right: 26 });
        assert_eq!(heard[1], Sample { left: 0, right: 0 });
        assert_eq!(alone.audio_samples().0, vec![0, 0]);
        assert_eq!(alone.audio_samples().1, vec![26, 0]);
        assert_eq!(live.audio_samples().0, Vec::<i16>::new());
        assert_eq!(live.audio_hz(), 262_144);
        assert_eq!(live.audio_overruns(), 0);
        assert_eq!(live.audio_produced(), 2);
    }

    #[test]
    fn output_volume_scales_stereo_samples_and_mute_outputs_silence() {
        let link = AudioLink::new(4);
        link.produce(
            Sample {
                left: 100,
                right: -100,
            },
            0,
        );
        let volume = AtomicU8::new(50);
        let mut state = OutputState::default();
        let mut output = [0i16; 2];
        fill_output(
            &mut output,
            2,
            link.hz(),
            &link,
            &volume,
            &mut state,
            |sample| sample,
        );
        assert_eq!(output, [3_200, -3_200]);

        link.produce(
            Sample {
                left: 100,
                right: -100,
            },
            0,
        );
        volume.store(0, Ordering::Relaxed);
        fill_output(
            &mut output,
            2,
            link.hz(),
            &link,
            &volume,
            &mut state,
            |sample| sample,
        );
        assert_eq!(output, [0, 0]);
    }
}
