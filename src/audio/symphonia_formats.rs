/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Decoding of miscellaneous formats (MP3, AAC) to linear PCM.
//!
//! This should be the only module in touchHLE that makes use of [symphonia].
//! For AAC, Only the LC profile and MPEG-4 container format are supported (see
//! feature list in Cargo.toml).
//!
//! Opening a file only demuxes it (to get the exact PCM length); a background
//! thread then decodes it into a window of a few seconds around the reads,
//! starting over from the beginning for a read behind the window (a loop),
//! and reads wait for the part they need. On the n72 (iPod touch 2G under Linux) MP3 goes to
//! the AMC audio coprocessor, see [super::amc].

use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use symphonia::core::codecs::audio::well_known::{
    CODEC_ID_AAC, CODEC_ID_ADPCM_IMA_QT, CODEC_ID_ADPCM_IMA_WAV, CODEC_ID_ALAC, CODEC_ID_MP3,
    CODEC_ID_PCM_S16LE,
};
use symphonia::core::codecs::audio::{AudioCodecId, AudioCodecParameters, AudioDecoder};
use symphonia::core::formats::FormatReader;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::packet::Packet;

/// The compressed file, shared by the readers (no copy per reader).
#[derive(Clone)]
struct SharedBytes(Arc<Vec<u8>>);
impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// PCM data decoded from a miscellaneous format file, as it is read.
pub struct SymphoniaDecodedToPcm {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u32,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

/// Decoded PCM kept ahead of the last read.
const AHEAD_SECONDS: usize = 4;
/// Decoded PCM kept behind the last read's start (re-reads of a buffer).
const BEHIND_SECONDS: usize = 1;
/// Frames decoded (and dropped) before the first frame that is kept, after
/// a jump: the decoder's state (MP3's bit reservoir, overlap) for that frame.
/// Three packets of the longest kind (ALAC: 4096 frames).
const PRIME_FRAMES: usize = 3 * 4096;

struct Shared {
    state: Mutex<State>,
    cond: Condvar,
    /// The announced length of the PCM: what the demuxer's packets add up to.
    byte_count: usize,
    /// Bytes per PCM frame.
    frame_bytes: usize,
    ahead: usize,
    behind: usize,
    prime: usize,
}

/// The decoded window. `buf` holds the stream's bytes `base..base +
/// buf.len()`. `produced` is the stream position of the decoder's next byte:
/// `base + buf.len()`, or less while it decodes towards a `base` it jumped to
/// (then `buf` is empty and what it produces below `base` is dropped).
struct State {
    buf: VecDeque<u8>,
    base: usize,
    produced: usize,
    /// The last read: `read_pos..read_end`.
    read_pos: usize,
    read_end: usize,
    /// A read wants bytes from before `base`: the decoder starts over.
    restart: bool,
    /// The decoder has gone through the stream (until a restart).
    finished: bool,
    stop: bool,
}

/// What the decoder does with its next packet.
enum Step {
    Decode,
    /// The packet ends well before what is wanted: counted, not decoded.
    Skipped,
    /// Far enough ahead (or a batch must end before a skip or a wait).
    Hold,
    Restart,
    Stop,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait<'a>(&self, st: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.cond.wait(st).unwrap_or_else(|e| e.into_inner())
    }

    fn keep_from(&self, st: &State) -> usize {
        st.read_pos.saturating_sub(self.behind)
    }

    /// Decide about a packet of `bytes` at stream position `pos`. `pos` is
    /// `produced` unless a batch (`batch_empty` false) is pending; only then
    /// it may differ, and the call never waits or skips.
    fn step(&self, pos: usize, bytes: usize, batch_empty: bool) -> Step {
        let mut st = self.lock();
        loop {
            if st.stop {
                return Step::Stop;
            }
            if st.restart {
                return Step::Restart;
            }
            let skippable = pos + bytes + self.prime <= self.keep_from(&st);
            let wanted = st
                .read_end
                .max(st.read_pos + self.ahead)
                .min(self.byte_count);
            if !batch_empty {
                return if skippable || pos >= wanted {
                    Step::Hold
                } else {
                    Step::Decode
                };
            }
            if skippable {
                st.buf.clear();
                st.produced += bytes;
                st.base = st.base.max(st.produced);
                return Step::Skipped;
            }
            if pos < wanted {
                return Step::Decode;
            }
            st = self.wait(st);
        }
    }

    /// Whether everything up to stream position `end` will be dropped.
    #[cfg(all(target_os = "linux", target_arch = "arm"))]
    fn dropped_below(&self, end: usize) -> bool {
        let st = self.lock();
        end <= st.base.max(self.keep_from(&st))
    }

    /// Append one packet's PCM: exactly `frames` frames, cut or padded with
    /// silence, so that every packet keeps its place.
    fn push(&self, pcm: &[u8], frames: u64) {
        let mut st = self.lock();
        let want = usize::try_from(frames)
            .unwrap_or(usize::MAX)
            .saturating_mul(self.frame_bytes)
            .min(self.byte_count.saturating_sub(st.produced));
        // Below `base`: dropped.
        let skip = st.base.saturating_sub(st.produced).min(want);
        let n = pcm.len().min(want);
        if skip < n {
            st.buf.extend(&pcm[skip..n]);
        }
        let zeros = want - n.max(skip);
        st.buf.extend(std::iter::repeat_n(0, zeros));
        st.produced += want;
        // Behind the reads: dropped.
        let gone = self
            .keep_from(&st)
            .saturating_sub(st.base)
            .min(st.buf.len());
        st.buf.drain(..gone);
        st.base += gone;
        drop(st);
        self.cond.notify_all();
    }

    /// The decoder went through the stream: wait for a restart. False to stop.
    fn finish_and_wait(&self) -> bool {
        let mut st = self.lock();
        st.finished = true;
        self.cond.notify_all();
        loop {
            if st.stop {
                return false;
            }
            if st.restart {
                return true;
            }
            st = self.wait(st);
        }
    }

    /// A new pass from the start of the stream, dropping up to a little before
    /// the last read.
    fn begin_pass(&self) {
        let mut st = self.lock();
        st.restart = false;
        st.finished = false;
        st.buf.clear();
        st.produced = 0;
        st.base = self.keep_from(&st);
    }
}

/// Marks the decode finished however the thread ends (a panic included), so
/// that no read waits forever.
struct FinishOnDrop(Arc<Shared>);
impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        let mut st = self.0.lock();
        st.finished = true;
        st.restart = false;
        st.stop = true;
        drop(st);
        self.0.cond.notify_all();
    }
}

fn open_reader(
    data: &SharedBytes,
) -> Result<(Box<dyn FormatReader>, u32, AudioCodecParameters), ()> {
    let mss = MediaSourceStream::new(Box::new(Cursor::new(data.clone())), Default::default());

    // If this failed, the container format is not supported.
    let probed = symphonia::default::get_probe()
        .probe(
            &Default::default(),
            mss,
            Default::default(),
            Default::default(),
        )
        .map_err(|_| ())?;

    let track = probed
        .tracks()
        .iter()
        .find(|t| {
            t.codec_params
                .as_ref()
                .and_then(|p| p.audio())
                .is_some_and(|p| {
                    [
                        CODEC_ID_AAC,
                        CODEC_ID_ADPCM_IMA_WAV,
                        CODEC_ID_ADPCM_IMA_QT,
                        CODEC_ID_ALAC,
                        CODEC_ID_MP3,
                        CODEC_ID_PCM_S16LE,
                    ]
                    .contains(&p.codec)
                })
        })
        .ok_or(())?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .unwrap()
        .audio()
        .unwrap()
        .clone();
    Ok((probed, track_id, params))
}

/// The next packet of the track. `Ok(None)` at the end of the stream; `Err`
/// for an error that is not an I/O error (I/O errors can only mean the end of
/// the file, because the entire file is in memory).
fn next_packet(reader: &mut dyn FormatReader, track_id: u32) -> Result<Option<Packet>, ()> {
    loop {
        match reader.next_packet() {
            Ok(Some(packet)) if packet.track_id == track_id => return Ok(Some(packet)),
            Ok(Some(_)) => continue,
            // "If Ok(None) is returned, the media has ended and no more
            // packets will be produced until the reader is seeked to a new
            // position."
            Ok(None) | Err(symphonia::core::errors::Error::IoError(_)) => return Ok(None),
            Err(_) => return Err(()),
        }
    }
}

/// PCM frames in an MPEG audio frame, from its header.
fn mpeg_frame_block(data: &[u8]) -> Option<u64> {
    let h = data.get(..4)?;
    if h[0] != 0xff || (h[1] & 0xe0) != 0xe0 {
        return None;
    }
    let mpeg1 = (h[1] >> 3) & 3 == 3;
    match (h[1] >> 1) & 3 {
        3 => Some(384),                            // Layer I
        2 => Some(1152),                           // Layer II
        1 => Some(if mpeg1 { 1152 } else { 576 }), // Layer III
        _ => None,
    }
}

/// What a track's codec is, as far as the frame count, the AMC and the
/// software decoder care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Mp3,
    /// AAC-LC: 1024 frames per packet. `amc`: 1 or 2 channels (what the AMC
    /// takes).
    Aac {
        amc: bool,
    },
    /// HE-AAC (SBR, v2 with `ps`): 2048 frames per packet at twice the core
    /// rate. Symphonia has no SBR: in software only the AAC-LC core decodes
    /// (`core`: its sampling frequency index and channels, if a plain AAC-LC
    /// config can say them).
    HeAac {
        ps: bool,
        rate: u32,
        channels: u32,
        core: Option<(u8, u8)>,
    },
    /// ALAC: bitDepth, pb, mb, kb from the 'alac' atom; `amc`: 16-bit, 4096
    /// frames per packet, 1 or 2 channels (what the AMC takes).
    Alac {
        cookie: [u8; 4],
        amc: bool,
    },
    Other,
}

const AAC_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

/// Big-endian bit reader over an AudioSpecificConfig.
struct Bits<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn remaining(&self) -> usize {
        (self.bytes.len() * 8).saturating_sub(self.pos)
    }
    fn peek(&self, n: usize) -> Option<u32> {
        if n > self.remaining() {
            return None;
        }
        let mut v = 0u32;
        for i in self.pos..self.pos + n {
            v = (v << 1) | u32::from((self.bytes[i / 8] >> (7 - i % 8)) & 1);
        }
        Some(v)
    }
    fn read(&mut self, n: usize) -> Option<u32> {
        let v = self.peek(n)?;
        self.pos += n;
        Some(v)
    }
    fn aot(&mut self) -> Option<u32> {
        let a = self.read(5)?;
        Some(if a == 31 { 32 + self.read(6)? } else { a })
    }
    /// Sampling frequency: (index, Hz).
    fn rate(&mut self) -> Option<(u32, u32)> {
        let i = self.read(4)?;
        if i == 15 {
            Some((15, self.read(24)?))
        } else {
            Some((i, *AAC_RATES.get(i as usize)?))
        }
    }
}

/// An AudioSpecificConfig, as gstamcdec reads it: the object type (after
/// the SBR/PS wrapper), the core rate and channels, and SBR/PS signalled
/// explicitly (object type 5 or 29) or backward compatibly (sync
/// extensions 0x2b7 and 0x548).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Asc {
    pub aot: u32,
    pub rate_index: u32,
    pub rate: u32,
    pub channels: u32,
    pub sbr: bool,
    pub ps: bool,
    /// The SBR (output) rate, if given.
    pub ext_rate: Option<u32>,
}

pub(super) fn parse_asc(bytes: &[u8]) -> Option<Asc> {
    let mut b = Bits { bytes, pos: 0 };
    let mut aot = b.aot()?;
    let (rate_index, rate) = b.rate()?;
    let channels = b.read(4)?;
    let (mut sbr, mut ps, mut ext_rate) = (false, false, None);
    if aot == 5 || aot == 29 {
        sbr = true;
        ps = aot == 29;
        ext_rate = Some(b.rate()?.1);
        aot = b.aot()?;
    }
    if aot == 2 && b.read(3).is_some() {
        // GASpecificConfig, then the extensions
        while b.remaining() >= 16 {
            match b.peek(11)? {
                0x2b7 => {
                    b.read(11)?;
                    if b.aot()? == 5 && b.read(1)? == 1 {
                        sbr = true;
                        ext_rate = Some(b.rate()?.1);
                    }
                }
                0x548 => {
                    b.read(11)?;
                    if b.read(1)? == 1 {
                        ps = true;
                    }
                }
                _ => break,
            }
        }
    }
    Some(Asc {
        aot,
        rate_index,
        rate,
        channels,
        sbr,
        ps,
        ext_rate,
    })
}

/// A plain AAC-LC AudioSpecificConfig.
fn lc_asc(rate_index: u8, channels: u8) -> [u8; 2] {
    let v: u16 = (2 << 11) | (u16::from(rate_index) << 7) | (u16::from(channels) << 3);
    v.to_be_bytes()
}

/// The 24-byte ALAC specific config in an 'alac' magic cookie (24 bytes, or
/// 48 with the 'frma' and 'alac' atom headers in front).
fn alac_config(cookie: &[u8]) -> Option<&[u8]> {
    match cookie.len() {
        24 => Some(cookie),
        48 if &cookie[4..8] == b"frma" => Some(&cookie[24..48]),
        _ => None,
    }
}

impl Kind {
    pub(super) fn of(params: &AudioCodecParameters) -> Kind {
        let extra = params.extra_data.as_deref();
        match params.codec {
            c if c == CODEC_ID_MP3 => Kind::Mp3,
            c if c == CODEC_ID_AAC => match extra.and_then(parse_asc) {
                Some(asc) if asc.aot == 2 && asc.sbr && (1..=2).contains(&asc.channels) => {
                    let core =
                        (asc.rate_index < 13).then_some((asc.rate_index as u8, asc.channels as u8));
                    Kind::HeAac {
                        ps: asc.ps,
                        rate: asc.ext_rate.unwrap_or(2 * asc.rate),
                        channels: if asc.ps { 2 } else { asc.channels },
                        core,
                    }
                }
                Some(asc) => Kind::Aac {
                    amc: asc.aot == 2 && (1..=2).contains(&asc.channels),
                },
                None => Kind::Aac { amc: false },
            },
            c if c == CODEC_ID_ALAC => {
                let Some(c) = extra.and_then(alac_config) else {
                    return Kind::Alac {
                        cookie: [0; 4],
                        amc: false,
                    };
                };
                let frame_length = u32::from_be_bytes(c[0..4].try_into().unwrap());
                Kind::Alac {
                    cookie: c[5..9].try_into().unwrap(),
                    amc: frame_length == 4096 && c[5] == 16 && (1..=2).contains(&c[9]),
                }
            }
            _ => Kind::Other,
        }
    }

    pub(super) fn name(&self, codec: AudioCodecId) -> &'static str {
        match self {
            Kind::Mp3 => "MP3",
            Kind::Aac { .. } => "AAC",
            Kind::HeAac { ps: false, .. } => "HE-AAC",
            Kind::HeAac { ps: true, .. } => "HE-AAC v2",
            Kind::Alac { .. } => "ALAC",
            Kind::Other if codec == CODEC_ID_ADPCM_IMA_WAV || codec == CODEC_ID_ADPCM_IMA_QT => {
                "IMA ADPCM"
            }
            Kind::Other => "PCM",
        }
    }

    /// The software decoder's parameters: HE-AAC as its AAC-LC core.
    fn soft_params(&self, params: &AudioCodecParameters) -> Option<AudioCodecParameters> {
        match *self {
            Kind::HeAac { core, .. } => {
                let (index, channels) = core?;
                let mut p = params.clone();
                p.extra_data = Some(Box::new(lc_asc(index, channels)));
                p.sample_rate = Some(AAC_RATES[index as usize]);
                Some(p)
            }
            _ => Some(params.clone()),
        }
    }
}

/// The frames a packet decodes to, as the decoders output them.
///
/// MP3: the decoder (gapless, symphonia's default) trims `trim_end` and then
/// `trim_start` frames from the frame's full block (the encoder delay and
/// padding of the LAME tag). The block comes from the header: symphonia
/// 0.6.0's MP3 demuxer leaves `dur` at the untrimmed block (its
/// `trimmed_dur` subtracts the builder's zero trims), so `dur` cannot be used
/// there. AAC: a whole frame for every packet, the last one too (its `dur`
/// may be shorter; neither the AMC nor symphonia cut it). The other demuxers
/// make untrimmed packets of `dur` frames, and their decoders do not trim.
fn packet_frames(packet: &Packet, kind: Kind) -> u64 {
    match kind {
        Kind::Mp3 => {
            let block = mpeg_frame_block(&packet.data).unwrap_or(packet.dur.get());
            block
                .saturating_sub(packet.trim_end.get())
                .saturating_sub(packet.trim_start.get())
        }
        Kind::Aac { .. } => 1024,
        Kind::HeAac { .. } => 2048,
        _ => packet.dur.get(),
    }
}

/// An untrimmed decoder's output for a packet (the AMC's), trimmed as
/// symphonia's gapless MP3 decoder does: `trim_end` frames off the end
/// first, then `trim_start` off the start.
#[cfg_attr(
    not(any(test, all(target_os = "linux", target_arch = "arm"))),
    allow(dead_code)
)]
fn trim_like_symphonia<'a>(pcm: &'a [u8], packet: &Packet, frame_bytes: usize) -> &'a [u8] {
    let block = (pcm.len() / frame_bytes) as u64;
    let end = block.saturating_sub(packet.trim_end.get());
    let start = packet.trim_start.get().min(end);
    &pcm[start as usize * frame_bytes..end as usize * frame_bytes]
}

fn make_decoder(params: &AudioCodecParameters) -> Result<Box<dyn AudioDecoder>, ()> {
    // Not sure why this would fail, maybe an unusual AAC track.
    symphonia::default::get_codecs()
        .make_audio_decoder(params, &Default::default())
        .map_err(|_| ())
}

/// Software decoding of one packet to S16_LE interleaved PCM at the stream's
/// rate and channels.
pub(super) struct Soft {
    decoder: Option<Box<dyn AudioDecoder>>,
    params: Option<AudioCodecParameters>,
    buf: Vec<u8>,
    out: Vec<u8>,
    pub rate: u32,
    pub channels: u32,
    /// HE-AAC's core: rate and channels the decoder gives, upsampled 2x
    /// (linear) and mono doubled to stereo (PS).
    core: Option<(u32, u32)>,
    /// The last core frame (interpolation across packets).
    last: [i16; 2],
}

impl Soft {
    /// `None` decoder: none can decode this (HE-AAC without a usable core).
    pub(super) fn new(kind: Kind, params: &AudioCodecParameters, rate: u32, channels: u32) -> Soft {
        let params = kind.soft_params(params);
        let core = match kind {
            Kind::HeAac {
                core: Some((index, ch)),
                ..
            } => Some((AAC_RATES[index as usize], u32::from(ch))),
            _ => None,
        };
        Soft {
            decoder: params.as_ref().and_then(|p| make_decoder(p).ok()),
            params,
            buf: Vec::new(),
            out: Vec::new(),
            rate,
            channels,
            core,
            last: [0; 2],
        }
    }

    pub(super) fn available(&self) -> bool {
        self.decoder.is_some()
    }

    /// Start over with a fresh decoder state (symphonia's `reset()` does not
    /// clear all of the AAC decoder's state).
    pub(super) fn fresh(&mut self) {
        if let Some(p) = &self.params {
            if let Ok(decoder) = make_decoder(p) {
                self.decoder = Some(decoder);
            }
        }
        self.last = [0; 2];
    }

    /// The packet's PCM, or `None` if it does not decode (to the stream's
    /// rate and channels).
    pub(super) fn decode(&mut self, packet: &Packet) -> Option<&[u8]> {
        let decoded = self.decoder.as_mut()?.decode(packet).ok()?;
        let spec = decoded.spec();
        let (rate, channels) = self.core.unwrap_or((self.rate, self.channels));
        if spec.rate() != rate || spec.channels().count() != channels as usize {
            return None;
        }
        self.buf.clear();
        decoded.copy_bytes_to_vec_interleaved_as::<i16>(&mut self.buf);
        if self.core.is_none() {
            return Some(&self.buf);
        }
        // HE-AAC core: twice the frames, by linear interpolation; mono to
        // both channels for PS.
        let (ci, co) = (channels as usize, self.channels as usize);
        self.out.clear();
        self.out.reserve(self.buf.len() * 2 * co / ci);
        for frame in self.buf.chunks_exact(2 * ci) {
            let mut now = [0i16; 2];
            for (c, s) in now.iter_mut().enumerate().take(ci) {
                *s = i16::from_le_bytes([frame[2 * c], frame[2 * c + 1]]);
            }
            if ci == 1 {
                now[1] = now[0];
            }
            let mid = [
                ((i32::from(self.last[0]) + i32::from(now[0])) / 2) as i16,
                ((i32::from(self.last[1]) + i32::from(now[1])) / 2) as i16,
            ];
            for f in [mid, now] {
                for s in f.iter().take(co) {
                    self.out.extend_from_slice(&s.to_le_bytes());
                }
            }
            self.last = now;
        }
        Some(&self.out)
    }
}

/// Sample rate and channels from the first packet that decodes, for tracks
/// whose parameters do not say.
fn spec_from_first_packet(data: &SharedBytes) -> Result<(u32, u32), ()> {
    let (mut reader, track_id, params) = open_reader(data)?;
    let mut decoder = make_decoder(&params)?;
    while let Some(packet) = next_packet(&mut *reader, track_id)? {
        if let Ok(decoded) = decoder.decode(&packet) {
            let spec = decoded.spec();
            return Ok((
                spec.rate(),
                spec.channels().count().try_into().map_err(|_| ())?,
            ));
        }
    }
    Err(())
}

/// The PCM frames of the track: a demux-only pass through symphonia.
fn frames_by_demuxing(data: &SharedBytes, kind: Kind) -> Result<u64, ()> {
    let (mut reader, track_id, _) = open_reader(data)?;
    let mut frames: u64 = 0;
    while let Some(packet) = next_packet(&mut *reader, track_id)? {
        frames += packet_frames(&packet, kind);
    }
    Ok(frames)
}

/// The PCM frames of an MP3 track as [frames_by_demuxing] counts them, from
/// the frame headers alone: the same frames as symphonia 0.6.0's MP3
/// demuxer (`read_mpeg_frame`: `sync_frame`, `parse_frame_header`, a frame
/// cut by the end of the file ends the stream) and the same trims as its
/// packets (`trimmed_dur`: the LAME delay as negative timestamps, the track's
/// frame count as the end). Layer I/II headers count too (junk can look like
/// one; the decoder turns them into silence). `None` for what this does not
/// mirror (a Xing/Info/VBRI tag after the first frame, which the demuxer
/// may drop): the demux pass then counts.
fn mp3_frames_by_hopping(reader: Box<dyn FormatReader>, bytes: &[u8]) -> Option<u64> {
    use symphonia::core::io::ReadBytes;

    let track = reader.tracks().first()?;
    let delay = u64::from(track.delay.unwrap_or(0));
    let end = track.num_frames;
    // Where the demuxer's first packet starts (after an ID3v2 tag and the
    // Info frame, which the probe and the reader have read).
    let mut p = usize::try_from(reader.into_inner().pos()).ok()?;

    // Stream position (in frames) of the next frame's start, plus `delay`.
    let mut at: u64 = 0;
    let mut frames: u64 = 0;
    loop {
        // sync_frame
        let mut sync = 0u32;
        loop {
            while sync & 0xffe0_0000 != 0xffe0_0000 {
                let Some(&b) = bytes.get(p) else {
                    return Some(frames);
                };
                sync = (sync << 8) | u32::from(b);
                p += 1;
            }
            let valid = (sync >> 19) & 3 != 1
                && (sync >> 17) & 3 != 0
                && (sync >> 12) & 0xf != 0xf
                && (sync >> 10) & 3 != 3;
            if valid {
                break;
            }
            let Some(&b) = bytes.get(p) else {
                return Some(frames);
            };
            sync = (sync << 8) | u32::from(b);
            p += 1;
        }
        // parse_frame_header
        let mpeg1 = (sync >> 19) & 3 == 3;
        // 3: Layer I, 2: Layer II, 1: Layer III
        let layer = (sync >> 17) & 3;
        let bitrate_idx = ((sync >> 12) & 0xf) as usize;
        if bitrate_idx == 0 {
            // Free bit-rate: not supported, the demuxer syncs on after it.
            continue;
        }
        let kbps: [u32; 15] = match (mpeg1, layer) {
            (true, 3) => [
                0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
            ],
            (true, 2) => [
                0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
            ],
            (true, _) => [
                0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
            ],
            (false, 3) => [
                0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
            ],
            (false, _) => [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        };
        let bitrate = 1000 * kbps[bitrate_idx];
        let mono = (sync >> 6) & 3 == 3;
        if layer == 2 {
            // Layer II channel mode and bit-rate combinations not allowed.
            let bad = if mono {
                [224_000, 256_000, 320_000, 384_000].contains(&bitrate)
            } else {
                [32_000, 48_000, 56_000, 80_000].contains(&bitrate)
            };
            if bad {
                continue;
            }
        }
        let rate = [44_100, 48_000, 32_000][((sync >> 10) & 3) as usize]
            >> match (sync >> 19) & 3 {
                3 => 0,
                2 => 1,
                _ => 2,
            };
        let padding = (sync >> 9) & 1;
        let frame_len = match layer {
            3 => (12 * bitrate / rate + padding) * 4,
            1 if !mpeg1 => 72 * bitrate / rate + padding,
            _ => 144 * bitrate / rate + padding,
        } as usize;
        let body = frame_len.checked_sub(4)?;
        let start = p - 4;
        if p + body > bytes.len() {
            // read_buf_exact fails: the stream has ended.
            return Some(frames);
        }
        let frame = &bytes[start..p + body];
        // A Xing/Info/VBRI tag here would be dropped by the demuxer after
        // a closer look: leave that to it.
        let side_info = match (mpeg1, mono) {
            (true, true) => 17,
            (true, false) => 32,
            (false, true) => 9,
            (false, false) => 17,
        };
        let tag_id = |at: usize| frame.get(at..at + 4);
        if matches!(tag_id(4 + side_info), Some(b"Xing" | b"Info")) || tag_id(36) == Some(b"VBRI") {
            return None;
        }
        p += body;

        // trimmed_dur, then the decoder's trim (end, then start)
        let block: u64 = match layer {
            3 => 384,
            2 => 1152,
            _ if mpeg1 => 1152,
            _ => 576,
        };
        let trim_start = delay.saturating_sub(at).min(block);
        let trim_end = match end {
            Some(end) => (at + block).saturating_sub(delay).saturating_sub(end),
            None => 0,
        };
        frames += block.saturating_sub(trim_end).saturating_sub(trim_start);
        at += block;
    }
}

pub fn decode_symphonia_to_pcm(bytes: Vec<u8>) -> Result<SymphoniaDecodedToPcm, ()> {
    let data = SharedBytes(Arc::new(bytes));

    // The exact length without decoding: MP3 by hopping over the frame
    // headers, the rest (or MP3 the hop does not understand) by a demux-only
    // pass.
    let (reader, track_id, params) = open_reader(&data)?;
    let kind = Kind::of(&params);
    let hopped = if kind == Kind::Mp3 {
        mp3_frames_by_hopping(reader, &data.0)
    } else {
        None
    };
    let frames = match hopped {
        Some(frames) => frames,
        None => frames_by_demuxing(&data, kind)?,
    };
    if frames == 0 {
        return Err(());
    }

    let (sample_rate, channels) = match (kind, params.sample_rate, params.channels.as_ref()) {
        // The output: the SBR rate.
        (Kind::HeAac { rate, channels, .. }, _, _) => (rate, channels),
        (_, Some(rate), Some(channels)) => (rate, channels.count().try_into().map_err(|_| ())?),
        _ => spec_from_first_packet(&data)?,
    };
    if sample_rate == 0 || channels == 0 {
        return Err(());
    }
    let frame_bytes = channels as usize * 2;
    let byte_count = usize::try_from(frames)
        .ok()
        .and_then(|f| f.checked_mul(frame_bytes))
        .ok_or(())?;
    let second = sample_rate as usize * frame_bytes;
    let ahead = AHEAD_SECONDS * second;
    let behind = BEHIND_SECONDS * second;

    let soft = Soft::new(kind, &params, sample_rate, channels);
    // Without a software decoder only HE-AAC opens (the AMC may take it).
    if !soft.available() && !matches!(kind, Kind::HeAac { .. }) {
        return Err(());
    }
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            // The window at once (plus a read and a batch of packets): no
            // reallocation (and copy) under the lock later.
            buf: VecDeque::with_capacity(byte_count.min(ahead + behind + second)),
            base: 0,
            produced: 0,
            read_pos: 0,
            read_end: 0,
            restart: false,
            finished: false,
            stop: false,
        }),
        cond: Condvar::new(),
        byte_count,
        frame_bytes,
        ahead,
        behind,
        prime: PRIME_FRAMES * frame_bytes,
    });
    let job = Decode {
        data,
        track_id,
        kind,
        name: kind.name(params.codec),
        frames,
        soft,
        shared: shared.clone(),
    };
    let thread = std::thread::Builder::new()
        .name("audio-decode".to_string())
        .spawn(move || job.run())
        .map_err(|_| ())?;

    Ok(SymphoniaDecodedToPcm {
        sample_rate,
        channels,
        shared,
        thread: Some(thread),
    })
}

impl SymphoniaDecodedToPcm {
    pub fn byte_count(&self) -> u64 {
        self.shared.byte_count as u64
    }

    /// Read `buffer.len()` bytes (or up to the end) from byte offset `offset`,
    /// waiting for the decoder if it has not got that far yet. Anything the
    /// decoder could not produce reads as silence.
    pub fn read_bytes(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, ()> {
        let byte_count = self.shared.byte_count;
        let offset = usize::try_from(offset).map_err(|_| ())?;
        if offset > byte_count {
            return Err(());
        }
        let n = buffer.len().min(byte_count - offset);
        let end = offset + n;
        let mut st = self.shared.lock();
        st.read_pos = offset;
        st.read_end = end;
        if offset < st.base {
            // Dropped already (a loop back to the start, a seek back).
            st.restart = true;
        }
        self.shared.cond.notify_all();
        while !st.stop && (st.restart || (st.produced < end && !st.finished)) {
            st = self.shared.wait(st);
        }
        let have = if st.restart || offset < st.base {
            0 // the decoder thread is gone
        } else {
            st.produced.min(end).saturating_sub(offset)
        };
        if have > 0 {
            let (a, b) = st.buf.as_slices();
            let start = offset - st.base;
            let from_a = a.len().saturating_sub(start).min(have);
            if from_a > 0 {
                buffer[..from_a].copy_from_slice(&a[start..][..from_a]);
            }
            let start_b = start + from_a - a.len().min(start + from_a);
            buffer[from_a..have].copy_from_slice(&b[start_b..][..have - from_a]);
        }
        drop(st);
        buffer[have..n].fill(0);
        Ok(n)
    }
}

impl Drop for SymphoniaDecodedToPcm {
    fn drop(&mut self) {
        // The thread looks at the flag between packets (AMC: between calls
        // of a few packets).
        self.shared.lock().stop = true;
        self.shared.cond.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Lower the calling thread's priority so that the game wins the CPU.
fn lower_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: plain system calls on this thread; a failure is harmless.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS as _, tid, 10);
    }
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
/// Packets the software decoder is primed with when it takes over from the
/// AMC mid-stream (MP3's bit reservoir and overlap), as in gstamcdec.
const HISTORY: usize = 3;

/// How a pass through the stream ended.
enum End {
    Done,
    Restart,
    Stop,
}

/// The decoder thread's job.
struct Decode {
    data: SharedBytes,
    track_id: u32,
    kind: Kind,
    name: &'static str,
    /// Announced length in frames.
    frames: u64,
    soft: Soft,
    shared: Arc<Shared>,
}

/// One pass's packets.
struct Packets {
    reader: Option<Box<dyn FormatReader>>,
    track_id: u32,
}

impl Packets {
    fn next(&mut self) -> Option<Packet> {
        // The demux pass went through the same packets: an error here ends
        // the stream where that pass ended it.
        next_packet(&mut **self.reader.as_mut()?, self.track_id)
            .ok()
            .flatten()
    }
}

/// The AMC's configuration for a stream, if it takes it.
#[cfg(all(target_os = "linux", target_arch = "arm"))]
fn amc_config(kind: Kind, rate: u32, channels: u32) -> Option<super::amc::AmcConfig> {
    use super::amc::{AmcCodec, AmcConfig, AMC_START_PS};
    let (codec, flags, cookie) = match kind {
        Kind::Mp3 => (AmcCodec::Mp3, 0, [0; 4]),
        Kind::Aac { amc: true } => (AmcCodec::Aac, 0, [0; 4]),
        Kind::HeAac { ps, .. } => (AmcCodec::HeAac, if ps { AMC_START_PS } else { 0 }, [0; 4]),
        Kind::Alac { cookie, amc: true } => (AmcCodec::Alac, 0, cookie),
        _ => return None,
    };
    Some(AmcConfig {
        codec,
        rate,
        channels,
        flags,
        cookie,
    })
}

impl Decode {
    fn describe(&self) -> String {
        format!(
            "{} Hz, {} ch, {} frames ({:.1} s)",
            self.soft.rate,
            self.soft.channels,
            self.frames,
            self.frames as f64 / f64::from(self.soft.rate)
        )
    }

    fn run(mut self) {
        let _finish = FinishOnDrop(self.shared.clone());
        lower_priority();

        // On the AMC until it fails once; then the CPU for good.
        #[allow(unused_mut)]
        let mut use_amc = cfg!(all(target_os = "linux", target_arch = "arm"));
        let mut first = true;
        loop {
            self.shared.begin_pass();
            let mut packets = Packets {
                reader: open_reader(&self.data).ok().map(|r| r.0),
                track_id: self.track_id,
            };
            if !first {
                self.soft.fresh();
            }
            let end = self.pass(&mut packets, &mut use_amc, first);
            first = false;
            match end {
                End::Stop => return,
                End::Restart => continue,
                End::Done => {
                    if !self.shared.finish_and_wait() {
                        return;
                    }
                }
            }
        }
    }

    fn pass(&mut self, packets: &mut Packets, use_amc: &mut bool, first: bool) -> End {
        #[allow(unused_mut)]
        let mut pending = Vec::new();
        #[allow(unused_mut)]
        let mut history = VecDeque::new();
        #[allow(unused_mut)]
        let mut pos = 0;

        // HE-AAC in software: the core only.
        let note = match self.kind {
            Kind::HeAac { .. } if self.soft.available() => {
                ", the AAC-LC core only (no SBR decoder), upsampled"
            }
            Kind::HeAac { .. } => ", no decoder for it: silence",
            _ => "",
        };
        #[cfg(all(target_os = "linux", target_arch = "arm"))]
        let config = amc_config(self.kind, self.soft.rate, self.soft.channels).filter(|_| *use_amc);
        #[cfg(all(target_os = "linux", target_arch = "arm"))]
        if let Some(config) = config {
            match super::amc::Amc::start(config) {
                Ok(amc) => {
                    if first {
                        log!("{} on the AMC, {}", self.name, self.describe());
                    }
                    match self.run_amc(amc, packets, &mut history) {
                        Ok(end) => return end,
                        Err((rest, why, at)) => {
                            log!(
                                "{}: the AMC stopped at frame {} ({}), software decoding from there{}",
                                self.name,
                                at / self.shared.frame_bytes,
                                why,
                                note
                            );
                            *use_amc = false;
                            pending = rest;
                            pos = at;
                        }
                    }
                }
                Err(why) => {
                    if first {
                        log!(
                            "{} in software (AMC: {}), {}{}",
                            self.name,
                            why,
                            self.describe(),
                            note
                        );
                    }
                }
            }
        } else if first {
            log!("{} in software, {}{}", self.name, self.describe(), note);
        }
        #[cfg(not(all(target_os = "linux", target_arch = "arm")))]
        if first {
            let _ = use_amc;
            log!("{} in software, {}{}", self.name, self.describe(), note);
        }

        self.run_soft(packets, pending, history, pos)
    }

    /// Software decoding of `pending` and the rest of the stream from stream
    /// position `pos`, the decoder first primed with `history` (output
    /// dropped).
    fn run_soft(
        &mut self,
        packets: &mut Packets,
        pending: Vec<Packet>,
        history: VecDeque<Packet>,
        mut pos: usize,
    ) -> End {
        if !self.soft.available() {
            // Nothing decodes it: the reads get silence.
            return End::Done;
        }
        for packet in &history {
            let _ = self.soft.decode(packet);
        }
        let mut pending = pending.into_iter();
        let mut gap = false;
        loop {
            let Some(packet) = pending.next().or_else(|| packets.next()) else {
                return End::Done;
            };
            let frames = packet_frames(&packet, self.kind);
            let bytes = frames as usize * self.shared.frame_bytes;
            match self.shared.step(pos, bytes, true) {
                Step::Stop => return End::Stop,
                Step::Restart => return End::Restart,
                Step::Skipped => gap = true,
                Step::Decode | Step::Hold => {
                    if gap {
                        self.soft.fresh();
                        gap = false;
                    }
                    // A packet that does not decode becomes silence, so that
                    // the rest stays in place.
                    let pcm = self.soft.decode(&packet).unwrap_or(&[]);
                    self.shared.push(pcm, frames);
                }
            }
            pos += bytes;
        }
    }

    /// The AMC. On a failure: the packets not decoded (the current one and
    /// the rest read ahead), why, and the stream position.
    #[cfg(all(target_os = "linux", target_arch = "arm"))]
    #[allow(clippy::type_complexity)]
    fn run_amc(
        &mut self,
        mut amc: super::amc::Amc,
        packets: &mut Packets,
        history: &mut VecDeque<Packet>,
    ) -> Result<End, (Vec<Packet>, String, usize)> {
        use super::amc::{AMC_MAX_INPUT, AMC_MAX_PACKETS};
        // Small calls: the first read (half a second) comes back soon. ALAC
        // one packet at a time: its frames per packet vary (the last one),
        // and the output comes back to back.
        const MAX_BATCH: usize = 16;
        const _: () = assert!(MAX_BATCH <= AMC_MAX_PACKETS);
        let kind = self.kind;
        let alac = matches!(kind, Kind::Alac { .. });
        let batch_max = if alac { 1 } else { MAX_BATCH };
        const BATCH: usize = MAX_BATCH;

        let fpp = u64::from(amc.frames_per_packet);
        let expected_fpp = match kind {
            Kind::Aac { .. } => Some(1024),
            Kind::HeAac { .. } => Some(2048),
            Kind::Alac { .. } => Some(4096),
            _ => None,
        };
        if expected_fpp.is_some_and(|f| f != fpp) {
            let first: Vec<Packet> = packets.next().into_iter().collect();
            return Err((first, format!("{fpp} frames per packet"), 0));
        }
        let frame_bytes = self.shared.frame_bytes;
        let packet_bytes = amc.frames_per_packet as usize * frame_bytes;
        let mut out = vec![0u8; packet_bytes * BATCH];
        let mut input = Vec::with_capacity(AMC_MAX_INPUT);
        let mut sizes = Vec::with_capacity(BATCH);
        let mut batch: Vec<Packet> = Vec::with_capacity(BATCH);
        // Packets to go (again) before the reader's next ones.
        let mut queue: VecDeque<Packet> = VecDeque::new();
        let mut next = packets.next();
        // Stream position of the next packet to go out (pushed or skipped).
        let mut pos = 0;
        let mut gap = false;

        loop {
            input.clear();
            sizes.clear();
            // Where the batch would end.
            let mut batch_end = pos;
            while let Some(packet) = next.take() {
                let len = packet.data.len();
                let fits = (kind != Kind::Mp3 || mpeg_frame_block(&packet.data) == Some(fpp))
                    && (!alac || packet.dur.get() <= fpp)
                    && len > 0
                    && input.len() + len <= AMC_MAX_INPUT;
                let bytes = packet_frames(&packet, kind) as usize * frame_bytes;
                if batch.len() == batch_max || !fits {
                    next = Some(packet);
                    break;
                }
                match self.shared.step(batch_end, bytes, batch.is_empty()) {
                    Step::Stop => return Ok(End::Stop),
                    Step::Restart => return Ok(End::Restart),
                    Step::Hold => {
                        next = Some(packet);
                        break;
                    }
                    Step::Skipped => {
                        gap = true;
                        pos += bytes;
                        batch_end = pos;
                    }
                    Step::Decode => {
                        input.extend_from_slice(&packet.data);
                        sizes.push(len as u32);
                        batch.push(packet);
                        batch_end += bytes;
                    }
                }
                next = queue.pop_front().or_else(|| packets.next());
            }
            if batch.is_empty() {
                // The stream has ended, or a packet the AMC cannot take
                // (another frame size): the CPU goes on from it.
                return match next {
                    None => Ok(End::Done),
                    Some(p) => Err((vec![p], "unsuitable packet".to_string(), pos)),
                };
            }
            if gap {
                // The AMC takes one continuous stream: after skipped packets,
                // a fresh start.
                amc.restart().map_err(|why| {
                    let mut rest: Vec<Packet> = std::mem::take(&mut batch);
                    rest.extend(next.take());
                    rest.extend(queue.drain(..));
                    (rest, why, pos)
                })?;
                gap = false;
            }
            let last = next.is_none();
            let r = amc.decode(&input, &sizes, &mut out[..packet_bytes * batch.len()], last);
            for (i, packet) in batch.drain(..r.packets_done).enumerate() {
                let pcm = if alac {
                    &out[..r.out_bytes]
                } else {
                    &out[i * packet_bytes..][..packet_bytes]
                };
                // MP3: as symphonia trims it (AAC packets have no trims).
                let pcm = trim_like_symphonia(pcm, &packet, frame_bytes);
                let frames = packet_frames(&packet, kind);
                self.shared.push(pcm, frames);
                pos += frames as usize * frame_bytes;
                if history.len() == HISTORY {
                    history.pop_front();
                }
                history.push_back(packet);
            }
            if let Some(why) = r.failure {
                if r.bitstream_error && !batch.is_empty() {
                    // A bad packet whose output is dropped anyway (the first
                    // ones after a jump, without their bit reservoir): start
                    // again after it.
                    let bad = &batch[0];
                    let frames = packet_frames(bad, kind);
                    let bad_end = pos + frames as usize * frame_bytes;
                    if self.shared.dropped_below(bad_end) && amc.restart().is_ok() {
                        batch.remove(0);
                        self.shared.push(&[], frames);
                        pos = bad_end;
                        history.clear();
                        // The rest of the batch goes again.
                        let mut again: VecDeque<Packet> = batch.drain(..).collect();
                        again.extend(next.take());
                        again.append(&mut queue);
                        queue = again;
                        next = queue.pop_front().or_else(|| packets.next());
                        continue;
                    }
                }
                drop(amc);
                let mut rest: Vec<Packet> = std::mem::take(&mut batch);
                rest.extend(next);
                rest.extend(queue);
                return Err((rest, why, pos));
            }
            if last {
                return Ok(End::Done);
            }
        }
    }
}

/// These need `lame` and `ffmpeg` (skipped without). The n72 fork does not
/// build for a desktop host (cpu-native), so run them through a scratch crate
/// that includes this file with `#[path]` and a `log!` macro.
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    /// The old whole-file decode, as the reference.
    fn decode_whole(bytes: Vec<u8>) -> Vec<u8> {
        let data = SharedBytes(Arc::new(bytes));
        let (mut reader, track_id, params) = open_reader(&data).unwrap();
        let mut decoder = make_decoder(&params).unwrap();
        let mut out = Vec::new();
        let mut buf = Vec::new();
        while let Some(packet) = next_packet(&mut *reader, track_id).unwrap() {
            let Ok(decoded) = decoder.decode(&packet) else {
                break;
            };
            buf.clear();
            decoded.copy_bytes_to_vec_interleaved_as::<i16>(&mut buf);
            out.extend_from_slice(&buf);
        }
        out
    }

    /// The AMC path's layout: untrimmed frames (as the AMC gives them, here
    /// from symphonia without gapless) trimmed by [trim_like_symphonia] must
    /// be the gapless decode, packet for packet.
    fn check_amc_layout(bytes: Vec<u8>, reference: &[u8]) {
        let data = SharedBytes(Arc::new(bytes));
        let (mut reader, track_id, params) = open_reader(&data).unwrap();
        let opts = symphonia::core::codecs::audio::AudioDecoderOptions::default().gapless(false);
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &opts)
            .unwrap();
        let mut out = Vec::new();
        let mut buf = Vec::new();
        while let Some(packet) = next_packet(&mut *reader, track_id).unwrap() {
            let decoded = decoder.decode(&packet).unwrap();
            let frame_bytes = decoded.spec().channels().count() * 2;
            buf.clear();
            decoded.copy_bytes_to_vec_interleaved_as::<i16>(&mut buf);
            assert_eq!(
                (buf.len() / frame_bytes) as u64,
                mpeg_frame_block(&packet.data).unwrap()
            );
            let trimmed = trim_like_symphonia(&buf, &packet, frame_bytes);
            assert_eq!(
                trimmed.len() as u64,
                packet_frames(&packet, Kind::Mp3) * frame_bytes as u64
            );
            out.extend_from_slice(trimmed);
        }
        assert!(out == reference);
    }

    fn write_wav(path: &Path, rate: u32, channels: u16, frames: u32) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..frames {
            for c in 0..channels {
                let t = f64::from(i) / f64::from(rate);
                let f = 440.0 * f64::from(c + 1) + 200.0 * t;
                w.write_sample((8000.0 * (2.0 * std::f64::consts::PI * f * t).sin()) as i16)
                    .unwrap();
            }
        }
        w.finalize().unwrap();
    }

    /// Encode with an external tool; None (test skipped) if it is missing.
    fn encode(tool: &str, args: &[&str], out: &Path) -> Option<Vec<u8>> {
        let status = Command::new(tool).args(args).status().ok()?;
        assert!(status.success(), "{tool} {args:?} failed");
        Some(std::fs::read(out).unwrap())
    }

    /// `exact`: restarts and jumps give the same bytes (MP3, ALAC; AAC is
    /// within 2 LSB there).
    fn check(bytes: Vec<u8>, mp3: bool, exact: bool) {
        let exact_length = exact;
        let reference = decode_whole(bytes.clone());
        let bytes_again = bytes.clone();
        if mp3 {
            check_amc_layout(bytes.clone(), &reference);
        }
        let pcm = decode_symphonia_to_pcm(bytes).unwrap();
        let len = pcm.byte_count() as usize;
        // The announced length is what the whole-file decode gave.
        assert_eq!(len, reference.len());
        // Reads of odd sizes from the start, while the thread decodes, then a
        // loop back to 0.
        let mut got = vec![0u8; len];
        let mut offset = 0;
        let mut step = 1;
        while offset < len {
            let n = (step * 4 + 6).min(len - offset);
            assert_eq!(
                pcm.read_bytes(offset as u64, &mut got[offset..][..n]),
                Ok(n)
            );
            offset += n;
            step = step * 7 % 997 + 1;
        }
        assert_eq!(got[..], reference[..len]);
        // The window stays bounded.
        {
            let st = pcm.shared.lock();
            let second = pcm.sample_rate as usize * pcm.channels as usize * 2;
            assert!(st.buf.len() <= (AHEAD_SECONDS + BEHIND_SECONDS + 1) * second);
        }
        // A loop back to 0, a jump forward, a seek back into the middle, a
        // re-read of the same buffer.
        let chunk = 4410 * 4;
        let frame = pcm.channels as usize * 2;
        for offset in [
            0,
            len * 3 / 4,
            len / 3,
            len / 3,
            len / 3 + chunk,
            len - 100,
            0,
        ] {
            let offset = offset / frame * frame;
            let n = chunk.min(len - offset);
            let mut again = vec![0u8; n];
            assert_eq!(pcm.read_bytes(offset as u64, &mut again), Ok(n));
            {
                // MP3 restarts are exact; AAC after a jump is within an LSB
                // (the decoder's state is not quite the same).
                let diff = again
                    .chunks(2)
                    .zip(reference[offset..][..n].chunks(2))
                    .map(|(a, b)| {
                        (i16::from_le_bytes([a[0], a[1]]) as i32
                            - i16::from_le_bytes([b[0], b[1]]) as i32)
                            .abs()
                    })
                    .max()
                    .unwrap_or(0);
                let tolerance = if exact_length { 0 } else { 2 };
                assert!(diff <= tolerance, "at {offset}: max diff {diff}");
            }
        }
        // All of it in one read (system sounds), after the jumps and fresh.
        let mut all = vec![0u8; len];
        assert_eq!(pcm.read_bytes(0, &mut all), Ok(len));
        assert!(all == reference[..len]);
        drop(pcm);
        let pcm = decode_symphonia_to_pcm(bytes_again).unwrap();
        let mut all = vec![0u8; len];
        assert_eq!(pcm.read_bytes(0, &mut all), Ok(len));
        assert!(all == reference[..len]);
        // Past the end: as before.
        let mut tail = [0u8; 8];
        assert_eq!(pcm.read_bytes(len as u64, &mut tail), Ok(0));
        assert_eq!(pcm.read_bytes(len as u64 + 2, &mut tail), Err(()));
    }

    #[test]
    fn incremental_matches_whole_file() {
        let dir = std::env::temp_dir().join(format!("touchhle-audio-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ran = 0;
        // Short ones fit in the window; the long one has jumps and restarts.
        for (rate, channels, seconds) in [(44100, 2, 3), (22050, 1, 3), (44100, 2, 15)] {
            let wav = dir.join(format!("in-{rate}-{seconds}.wav"));
            write_wav(&wav, rate, channels, rate * seconds + 123);
            let wav_s = wav.to_str().unwrap();
            // With the LAME (Info) tag: gapless trimming.
            let mp3 = dir.join(format!("tag-{rate}-{seconds}.mp3"));
            if let Some(b) = encode("lame", &["--quiet", wav_s, mp3.to_str().unwrap()], &mp3) {
                check(b, true, true);
                ran += 1;
            }
            // Without it, VBR.
            let mp3 = dir.join(format!("notag-{rate}-{seconds}.mp3"));
            if let Some(b) = encode(
                "lame",
                &["--quiet", "-t", "-V", "4", wav_s, mp3.to_str().unwrap()],
                &mp3,
            ) {
                check(b, true, true);
                ran += 1;
            }
            let m4a = dir.join(format!("aac-{rate}-{seconds}.m4a"));
            if let Some(b) = encode(
                "ffmpeg",
                &[
                    "-loglevel",
                    "error",
                    "-y",
                    "-i",
                    wav_s,
                    "-c:a",
                    "aac",
                    m4a.to_str().unwrap(),
                ],
                &m4a,
            ) {
                check(b, false, false);
                ran += 1;
            }
            let alac = dir.join(format!("alac-{rate}-{seconds}.m4a"));
            if let Some(b) = encode(
                "ffmpeg",
                &[
                    "-loglevel",
                    "error",
                    "-y",
                    "-i",
                    wav_s,
                    "-c:a",
                    "alac",
                    alac.to_str().unwrap(),
                ],
                &alac,
            ) {
                let data = SharedBytes(Arc::new(b.clone()));
                let (_, _, params) = open_reader(&data).unwrap();
                assert!(
                    matches!(Kind::of(&params), Kind::Alac { amc: true, .. }),
                    "{:?}",
                    Kind::of(&params)
                );
                check(b, false, true);
                ran += 1;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        if ran == 0 {
            eprintln!("lame/ffmpeg not found: nothing tested");
        }
    }

    #[test]
    fn asc() {
        // AAC-LC 44.1 kHz stereo
        let lc = parse_asc(&[0x12, 0x10]).unwrap();
        assert_eq!((lc.aot, lc.rate, lc.channels, lc.sbr), (2, 44100, 2, false));
        assert_eq!(lc_asc(4, 2), [0x12, 0x10]);
        // HE-AAC explicit: object type 5, 22.05 kHz core, stereo, SBR at
        // 44.1 kHz, then AAC-LC.
        let he = parse_asc(&[0x2b, 0x92, 0x08, 0x00]).unwrap();
        assert_eq!(
            (he.aot, he.rate, he.channels, he.sbr, he.ps, he.ext_rate),
            (2, 22050, 2, true, false, Some(44100))
        );
        // HE-AAC v2 explicit: object type 29, mono core.
        let v2 = parse_asc(&[0xeb, 0x8a, 0x08, 0x00]).unwrap();
        assert_eq!((v2.aot, v2.channels, v2.sbr, v2.ps), (2, 1, true, true));
        // Backward-compatible SBR: AAC-LC 22.05 kHz stereo, sync extension
        // 0x2b7, object type 5, SBR present, 44.1 kHz.
        let bc = parse_asc(&[0x13, 0x90, 0x56, 0xe5, 0xa0]).unwrap();
        assert_eq!(
            (bc.aot, bc.rate, bc.sbr, bc.ext_rate),
            (2, 22050, true, Some(44100))
        );
    }

    /// HE-AAC (afconvert on macOS): the length at the SBR rate, 2048 frames
    /// per packet; in software the AAC-LC core, upsampled.
    #[test]
    fn he_aac_core() {
        let dir = std::env::temp_dir().join(format!("touchhle-audio-he-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("in.wav");
        write_wav(&wav, 44100, 2, 44100 * 5);
        for (format, ps) in [("aach", false), ("aacp", true)] {
            let m4a = dir.join(format!("{format}.m4a"));
            let Some(b) = encode(
                "afconvert",
                &[
                    "-f",
                    "m4af",
                    "-d",
                    format,
                    wav.to_str().unwrap(),
                    m4a.to_str().unwrap(),
                ],
                &m4a,
            ) else {
                continue;
            };
            let data = SharedBytes(Arc::new(b.clone()));
            let (mut reader, track_id, params) = open_reader(&data).unwrap();
            let kind = Kind::of(&params);
            assert!(
                matches!(kind, Kind::HeAac { ps: p, rate: 44100, channels: 2, core: Some(_) } if p == ps),
                "{kind:?}"
            );
            let mut packets = 0u64;
            while next_packet(&mut *reader, track_id).unwrap().is_some() {
                packets += 1;
            }
            let pcm = decode_symphonia_to_pcm(b).unwrap();
            assert_eq!((pcm.sample_rate, pcm.channels), (44100, 2));
            assert_eq!(pcm.byte_count(), packets * 2048 * 4);
            let mut all = vec![0u8; pcm.byte_count() as usize];
            assert_eq!(pcm.read_bytes(0, &mut all), Ok(all.len()));
            // The core decodes: sound in the middle.
            let mid = &all[all.len() / 2..][..4096];
            assert!(mid
                .chunks(2)
                .any(|s| i16::from_le_bytes([s[0], s[1]]).abs() > 1000));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The header hop counts what the demux pass counts.
    #[test]
    fn mp3_hop_matches_demux() {
        let dir = std::env::temp_dir().join(format!("touchhle-audio-hop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ran = 0;
        for (rate, channels) in [(44100, 2), (22050, 1), (22050, 2)] {
            let wav = dir.join(format!("in-{rate}-{channels}.wav"));
            write_wav(&wav, rate, channels, rate * 7 + 321);
            let wav_s = wav.to_str().unwrap();
            let variants: [(&str, &[&str]); 5] = [
                ("cbr", &["-b", "128", "--cbr"]),
                ("vbr", &["-V", "4"]),
                ("vbr-notag", &["-t", "-V", "4"]),
                ("cbr-notag", &["-t", "-b", "64", "--cbr"]),
                (
                    "id3v2",
                    &["--id3v2-only", "--tt", "Title", "--ta", "Artist"],
                ),
            ];
            for (name, opts) in variants {
                let mp3 = dir.join(format!("{name}-{rate}-{channels}.mp3"));
                let mut args = vec!["--quiet"];
                args.extend_from_slice(opts);
                args.extend([wav_s, mp3.to_str().unwrap()]);
                let Some(mut bytes) = encode("lame", &args, &mp3) else {
                    continue;
                };
                for junk in [false, true] {
                    if junk {
                        // An ID3v1 tag and junk with sync-like bytes.
                        bytes.extend_from_slice(b"TAG");
                        bytes.resize(bytes.len() + 125, b' ');
                        let mut x = 0x1234_5678u32;
                        for i in 0..3000 {
                            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                            bytes.push(if i % 97 == 0 { 0xff } else { (x >> 16) as u8 });
                        }
                    }
                    let data = SharedBytes(Arc::new(bytes.clone()));
                    let demuxed = frames_by_demuxing(&data, Kind::Mp3).unwrap();
                    let (reader, _, _) = open_reader(&data).unwrap();
                    let hopped = mp3_frames_by_hopping(reader, &bytes);
                    assert_eq!(
                        hopped,
                        Some(demuxed),
                        "{name} {rate} {channels} junk {junk}"
                    );
                    ran += 1;
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        if ran == 0 {
            eprintln!("lame not found: nothing tested");
        }
    }

    #[test]
    fn drop_while_decoding() {
        let dir = std::env::temp_dir().join(format!("touchhle-audio-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("in.wav");
        write_wav(&wav, 44100, 2, 44100 * 20);
        let mp3 = dir.join("out.mp3");
        let b = encode(
            "lame",
            &["--quiet", wav.to_str().unwrap(), mp3.to_str().unwrap()],
            &mp3,
        );
        let _ = std::fs::remove_dir_all(&dir);
        let Some(b) = b else { return };
        let pcm = decode_symphonia_to_pcm(b).unwrap();
        let mut buf = [0u8; 16];
        pcm.read_bytes(0, &mut buf).unwrap();
        let t = std::time::Instant::now();
        drop(pcm);
        assert!(t.elapsed() < std::time::Duration::from_millis(500));
    }
}
