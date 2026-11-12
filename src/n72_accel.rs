/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! n72 fork: the iPod touch 2G's accelerometer (ST LIS3LV02DL) through
//! Linux evdev. SDL2 has no Linux sensor backend, so [crate::window] asks
//! this module when there is no SDL sensor.
//!
//! The lis3lv02d input device reports the current position in milli-g on
//! ABS_X/Y/Z and is polled by the kernel while it is open. The axes and
//! signs that match UIAcceleration (screen x right, y up, z out of the
//! screen, (0, 0, -1) lying flat on its back) depend on how the chip sits on
//! the board; `N72_ACCEL_MAP` overrides the default, e.g. "-y,x,-z".

use std::fs::File;
use std::os::fd::AsRawFd;

pub struct Accelerometer {
    file: File,
    /// For each UIAcceleration axis: (evdev axis 0..=2, sign).
    map: [(usize, f32); 3],
}

#[repr(C)]
#[derive(Default)]
struct InputAbsinfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

/// EVIOCGABS(abs) = _IOR('E', 0x40 + abs, struct input_absinfo)
fn eviocgabs(abs: u32) -> u32 {
    (2 << 30) | ((std::mem::size_of::<InputAbsinfo>() as u32) << 16) | (0x45 << 8) | (0x40 + abs)
}

/// The default mapping assumes the chip's axes follow the screen and that,
/// like SDL, it reports the reaction to gravity (flat on its back: z = +1 g).
const DEFAULT_MAP: [(usize, f32); 3] = [(0, -1.0), (1, -1.0), (2, -1.0)];

fn parse_map(s: &str) -> Option<[(usize, f32); 3]> {
    let mut out = [(0, 1.0); 3];
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.len() != 3 {
        return None;
    }
    for (i, p) in parts.iter().enumerate() {
        let (sign, axis) = match p.strip_prefix('-') {
            Some(rest) => (-1.0, rest),
            None => (1.0, p.strip_prefix('+').unwrap_or(p)),
        };
        let axis = match axis {
            "x" => 0,
            "y" => 1,
            "z" => 2,
            _ => return None,
        };
        out[i] = (axis, sign);
    }
    Some(out)
}

impl Accelerometer {
    /// Find the accelerometer among /dev/input/event*.
    pub fn open() -> Option<Accelerometer> {
        for entry in std::fs::read_dir("/sys/class/input").ok()?.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("event") {
                continue;
            }
            let dev_name = std::fs::read_to_string(entry.path().join("device/name")).ok()?;
            if !dev_name.contains("Accelerometer") {
                continue;
            }
            let file = File::open(format!("/dev/input/{name}")).ok()?;
            let map = match std::env::var("N72_ACCEL_MAP") {
                Ok(s) => parse_map(&s).unwrap_or_else(|| {
                    log!("Ignoring malformed N72_ACCEL_MAP={:?}", s);
                    DEFAULT_MAP
                }),
                Err(_) => DEFAULT_MAP,
            };
            log!(
                "Using the device accelerometer \"{}\" (/dev/input/{})",
                dev_name.trim(),
                name
            );
            return Some(Accelerometer { file, map });
        }
        None
    }

    /// Current acceleration in g, as UIAcceleration reports it.
    pub fn get(&self) -> (f32, f32, f32) {
        let mut raw = [0f32; 3];
        for (axis, value) in raw.iter_mut().enumerate() {
            let mut info = InputAbsinfo::default();
            let res = unsafe {
                libc::ioctl(
                    self.file.as_raw_fd(),
                    eviocgabs(axis as u32) as _,
                    &mut info as *mut InputAbsinfo,
                )
            };
            if res == 0 {
                *value = info.value as f32 / 1000.0;
            }
        }
        let pick = |(axis, sign): (usize, f32)| raw[axis] * sign;
        (pick(self.map[0]), pick(self.map[1]), pick(self.map[2]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn map_parsing() {
        assert_eq!(parse_map("-y,x,-z"), Some([(1, -1.0), (0, 1.0), (2, -1.0)]));
        assert_eq!(parse_map("x,y"), None);
        assert_eq!(parse_map("x,y,w"), None);
    }
}
