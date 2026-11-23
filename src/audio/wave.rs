/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! WAVE files through [hound], with plain 8-bit and 16-bit PCM copied
//! straight out of the data chunk.

use std::io::Cursor;

pub struct WaveFile {
    bytes: Vec<u8>,
    pub spec: hound::WavSpec,
    /// Samples (not frames) in the data chunk.
    pub num_samples: u32,
    /// Where the data chunk's samples start in `bytes`, if they can be
    /// copied as they are (8 or 16 bits stored in as many bytes).
    raw_data_offset: Option<usize>,
}

impl WaveFile {
    pub fn new(bytes: Vec<u8>) -> Result<WaveFile, ()> {
        let reader = hound::WavReader::new(Cursor::new(&bytes[..])).map_err(|_| ())?;
        let spec = reader.spec();
        let num_samples = reader.len();
        // The reader stops right at the data chunk's start; the chunk's
        // length is the four bytes before it.
        let data_offset = reader.into_inner().position() as usize;
        let data_len = data_offset
            .checked_sub(4)
            .and_then(|o| bytes.get(o..data_offset))
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let raw = spec.sample_format == hound::SampleFormat::Int
            && matches!(spec.bits_per_sample, 8 | 16)
            && data_len.map(u64::from)
                == Some(u64::from(num_samples) * u64::from(spec.bits_per_sample / 8));
        Ok(WaveFile {
            bytes,
            spec,
            num_samples,
            raw_data_offset: raw.then_some(data_offset),
        })
    }

    /// As [crate::audio::AudioFile::read_bytes] for a WAVE file (8-bit samples
    /// unsigned, 16-bit signed little-endian): reads from the start of the
    /// frame `offset` falls in, up to the end of the data.
    pub fn read_bytes(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, ()> {
        let bytes_per_sample = u64::from(self.spec.bits_per_sample / 8);
        assert!(offset.is_multiple_of(bytes_per_sample));
        assert!(u64::try_from(buffer.len())
            .unwrap()
            .is_multiple_of(bytes_per_sample));
        let Some(data_offset) = self.raw_data_offset else {
            return self.read_bytes_by_samples(offset, buffer);
        };
        // hound's seek is to a whole frame; the samples then run to the end.
        let frame = bytes_per_sample * u64::from(self.spec.channels);
        let start = offset / frame * frame;
        let data_len = u64::from(self.num_samples) * bytes_per_sample;
        let n = (buffer.len() as u64).min(data_len.saturating_sub(start)) as usize;
        if n == 0 {
            return Ok(0);
        }
        let start = data_offset + start as usize;
        // A data chunk cut short by the end of the file: an error, as reading
        // the missing samples is.
        let src = self.bytes.get(start..start + n).ok_or(())?;
        buffer[..n].copy_from_slice(src);
        Ok(n)
    }

    /// The general case: sample by sample through hound.
    fn read_bytes_by_samples(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, ()> {
        let bytes_per_sample = u64::from(self.spec.bits_per_sample / 8);
        let sample_count = u64::try_from(buffer.len()).unwrap() / bytes_per_sample;
        let sample_count: usize = sample_count.try_into().unwrap();

        let mut wave_reader =
            hound::WavReader::new(Cursor::new(&self.bytes[..])).map_err(|_| ())?;
        let channels: u64 = wave_reader.spec().channels.into();
        // WavReader expects number of samples which are
        // independent of the number of channels here
        wave_reader
            .seek((offset / (bytes_per_sample * channels)).try_into().unwrap())
            .map_err(|_| ())?;

        let mut byte_offset = 0;
        for sample in wave_reader.samples().take(sample_count) {
            let sample: i16 = sample.map_err(|_| ())?;
            match bytes_per_sample {
                // From the OpenAL docs: 8-bit PCM data is expressed as
                // an unsigned value over the range 0 to 255, 128 being
                // an audio output level of zero. Loaded wav samples
                // must be converted to that from signed with 0 as
                // output level 0.
                1 => buffer[byte_offset] = (sample + 128) as u8,
                2 => buffer[byte_offset..][..2].copy_from_slice(&sample.to_le_bytes()),
                _ => todo!(),
            }
            byte_offset += bytes_per_sample as usize;
        }
        Ok(byte_offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(bits: u16, channels: u16, frames: u32, extra_chunk: bool) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels,
            sample_rate: 22050,
            bits_per_sample: bits,
            sample_format: hound::SampleFormat::Int,
        };
        let mut out = Cursor::new(Vec::new());
        let mut w = hound::WavWriter::new(&mut out, spec).unwrap();
        let mut x = 7u32;
        for _ in 0..frames * u32::from(channels) {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            if bits == 8 {
                w.write_sample((x >> 24) as i8).unwrap();
            } else {
                w.write_sample((x >> 16) as i16).unwrap();
            }
        }
        w.finalize().unwrap();
        let mut bytes = out.into_inner();
        if extra_chunk {
            // A LIST chunk before the data: the data offset moves.
            let fmt_end = 12 + 8 + 16;
            let list = [b"LIST".as_slice(), &6u32.to_le_bytes(), b"INFOab"].concat();
            bytes.splice(fmt_end..fmt_end, list);
            let riff_len = (bytes.len() - 8) as u32;
            bytes[4..8].copy_from_slice(&riff_len.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn raw_copy_matches_hound() {
        for bits in [8, 16] {
            for channels in [1, 2] {
                for extra in [false, true] {
                    let file = WaveFile::new(wav(bits, channels, 1001, extra)).unwrap();
                    assert!(file.raw_data_offset.is_some());
                    let bps = usize::from(bits / 8);
                    let total = file.num_samples as usize * bps;
                    for offset in (0..total + 3 * bps)
                        .step_by(bps * 37)
                        .chain([total, total + bps])
                    {
                        for len in [0, bps, 3 * bps, 100 * bps, total + 8 * bps] {
                            let mut a = vec![0xaa; len];
                            let mut b = vec![0xaa; len];
                            let ra = file.read_bytes(offset as u64, &mut a);
                            let rb = file.read_bytes_by_samples(offset as u64, &mut b);
                            assert_eq!(ra, rb, "{bits} {channels} {offset} {len}");
                            assert_eq!(a, b, "{bits} {channels} {offset} {len}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn truncated_data_chunk() {
        let mut bytes = wav(16, 2, 100, false);
        bytes.truncate(bytes.len() - 10);
        // hound refuses or reads up to the cut: either way the same.
        if let Ok(file) = WaveFile::new(bytes) {
            for offset in [0u64, 360, 392] {
                let mut a = vec![0; 40];
                let mut b = vec![0; 40];
                assert_eq!(
                    file.read_bytes(offset, &mut a).is_ok(),
                    file.read_bytes_by_samples(offset, &mut b).is_ok()
                );
            }
        }
    }
}
