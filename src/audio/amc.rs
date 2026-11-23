/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! n72 (iPod touch 2G under Linux): MP3 decoding on the AMC audio coprocessor
//! through `/dev/amc` (kernel driver s5l8720-amc, interface in the kernel's
//! `include/uapi/linux/s5l8720-amc.h`).
//!
//! One packet (a whole MP3 frame, header included) in, one frame of
//! `frames_per_packet` S16_LE interleaved PCM frames out, with no delay
//! handling: the output is the plain decoder output, like an untrimmed
//! software decoder's.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;

const AMC_CODEC_MP3: u32 = 0;
const AMC_DECODE_DRAIN: u32 = 1 << 0;
/// Limits of one `AMC_IOC_DECODE` call.
pub const AMC_MAX_INPUT: usize = 0x8000;
pub const AMC_MAX_PACKETS: usize = 256;

#[repr(C)]
#[derive(Default)]
struct AmcStart {
    codec: u32,
    rate: u32,
    channels: u32,
    flags: u32,
    cookie: [u8; 4],
    // returned
    frames_per_packet: u32,
    bitrate: u32,
    reserved: [u32; 3],
}

#[repr(C)]
#[derive(Default)]
struct AmcDecode {
    input: u64,
    sizes: u64,
    out: u64,
    npackets: u32,
    out_size: u32,
    flags: u32,
    // returned
    packets_done: u32,
    out_bytes: u32,
    error: i32,
    reserved: [u32; 4],
}

// Same layout as the C structs (the u64 fields are 8-aligned on ARM EABI).
const _: () = assert!(std::mem::size_of::<AmcStart>() == 40);
const _: () = assert!(std::mem::size_of::<AmcDecode>() == 64);

/// `_IOWR(0xA6, nr, size)`
const fn iowr(nr: u32, size: usize) -> u32 {
    (3 << 30) | ((size as u32) << 16) | (0xA6 << 8) | nr
}
const AMC_IOC_START: u32 = iowr(0, std::mem::size_of::<AmcStart>());
const AMC_IOC_DECODE: u32 = iowr(1, std::mem::size_of::<AmcDecode>());

/// A started MP3 stream on the coprocessor. Dropping it closes `/dev/amc`.
pub struct Amc {
    file: File,
    rate: u32,
    channels: u32,
    /// PCM frames per packet, as the driver reports it.
    pub frames_per_packet: u32,
}

/// What one [Amc::decode] call did.
pub struct Decoded {
    /// Packets decoded, each `frames_per_packet` frames of output.
    pub packets_done: usize,
    /// Why the call stopped before the last packet, if it did. The stream
    /// needs a new start afterwards.
    pub failure: Option<String>,
    /// The failure is the decoder's (a bad packet), not the driver's or
    /// the coprocessor being busy.
    pub bitstream_error: bool,
}

fn errno_text() -> String {
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EBUSY) {
        "busy".to_string()
    } else {
        err.to_string()
    }
}

impl Amc {
    pub fn start_mp3(rate: u32, channels: u32) -> Result<Amc, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/amc")
            .map_err(|e| format!("/dev/amc: {e}"))?;
        let mut amc = Amc {
            file,
            rate,
            channels,
            frames_per_packet: 0,
        };
        amc.restart()?;
        Ok(amc)
    }

    /// Start the stream (again): a fresh decoder state, e.g. after a gap in
    /// the packets or a decoder error.
    pub fn restart(&mut self) -> Result<(), String> {
        let mut s = AmcStart {
            codec: AMC_CODEC_MP3,
            rate: self.rate,
            channels: self.channels,
            ..Default::default()
        };
        // SAFETY: the struct matches the driver's struct amc_start.
        if unsafe { libc::ioctl(self.file.as_raw_fd(), AMC_IOC_START as _, &mut s) } != 0 {
            return Err(format!("start: {}", errno_text()));
        }
        self.frames_per_packet = s.frames_per_packet;
        Ok(())
    }

    /// Decode `sizes.len()` packets (at most [AMC_MAX_PACKETS], together at
    /// most [AMC_MAX_INPUT] bytes) lying back to back in `input` into `out`,
    /// which must hold `frames_per_packet` frames per packet. `drain` marks
    /// the last call of the stream.
    pub fn decode(&mut self, input: &[u8], sizes: &[u32], out: &mut [u8], drain: bool) -> Decoded {
        let mut d = AmcDecode {
            input: input.as_ptr() as usize as u64,
            sizes: sizes.as_ptr() as usize as u64,
            out: out.as_mut_ptr() as usize as u64,
            npackets: sizes.len() as u32,
            out_size: out.len() as u32,
            flags: if drain { AMC_DECODE_DRAIN } else { 0 },
            // On EBUSY the driver hands these back unchanged.
            packets_done: 0,
            out_bytes: 0,
            ..Default::default()
        };
        // SAFETY: the struct matches the driver's struct amc_decode, and the
        // buffers it points to are valid for the sizes given.
        let ret = unsafe { libc::ioctl(self.file.as_raw_fd(), AMC_IOC_DECODE as _, &mut d) };
        let failure = if ret != 0 {
            Some(errno_text())
        } else if d.error != 0 {
            Some(format!("decoder error {}", d.error))
        } else if d.packets_done as usize != sizes.len() {
            Some(format!("{} of {} packets", d.packets_done, sizes.len()))
        } else {
            None
        };
        Decoded {
            packets_done: (d.packets_done as usize).min(sizes.len()),
            bitstream_error: ret == 0 && d.error != 0,
            failure,
        }
    }
}
