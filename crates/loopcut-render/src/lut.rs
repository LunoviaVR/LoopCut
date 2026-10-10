//! Adobe/IRIDAS `.cube` LUT parsing, plus a CPU reference sampler that matches the GPU lookup.
//!
//! Accepted syntax, one statement per line:
//! * `TITLE "name"` (quotes optional).
//! * `LUT_1D_SIZE n` or `LUT_3D_SIZE n`, exactly once, before the data. Sizes: 1D 2 to 65536, 3D 2 to 256.
//! * `DOMAIN_MIN r g b` and `DOMAIN_MAX r g b` (defaults 0 0 0 and 1 1 1), with every channel of the minimum below
//!   the maximum. The IRIDAS aliases `LUT_1D_INPUT_RANGE lo hi` and `LUT_3D_INPUT_RANGE lo hi` set both domains to
//!   the same range for all channels.
//! * Data rows of three finite floats. A 3D table has N^3 rows with red varying fastest, then green, then blue. A
//!   1D table has N rows, and channel c of the output is read from column c.
//!
//! `#` starts a comment that runs to the end of the line, except on a `TITLE` line, where the whole value is the
//! title and may contain `#`. Blank lines are ignored, and a leading UTF-8 byte-order mark is skipped.
//! [`load_cube`] refuses files over [`MAX_LUT_FILE_BYTES`], and anything that is not a regular file.
//!
//! The compositor samples 3D tables with tetrahedral interpolation on an `rgba16float` 3D texture (`Lut::rgba16f`
//! is that upload). [`Lut::sample`] runs the same arithmetic on the CPU in `f32` and is the reference for the tests.

use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use thiserror::Error;

/// Largest accepted `LUT_3D_SIZE`. A 256-cube uploads as 134 MB of `rgba16float`.
pub const MAX_3D_SIZE: u32 = 256;
/// Largest accepted `LUT_1D_SIZE`.
pub const MAX_1D_SIZE: u32 = 65_536;
/// Largest `.cube` file that [`load_cube`] reads, in bytes (64 MiB). Every caller that loads a LUT (export, preview and
/// the app shell) goes through [`load_cube`], so this is the one limit. A 7-digit 3D table of 128 cubed is about 62 MiB,
/// so the usual sizes fit.
pub const MAX_LUT_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Source of [`Lut`] identities. Two parses never share an id, so a cached upload can tell tables apart.
static NEXT_LUT_ID: AtomicU64 = AtomicU64::new(1);

/// A parsed `.cube` lookup table (1D or 3D). Cloning is cheap: clones share the table.
#[derive(Clone)]
pub struct Lut {
    data: Arc<LutData>,
}

impl fmt::Debug for Lut {
    /// Shows the header and the entry count only, never the table itself.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lut")
            .field("id", &self.data.id)
            .field("title", &self.data.title)
            .field("size", &self.data.size)
            .field("is_3d", &self.data.is_3d)
            .field("domain_min", &self.data.domain_min)
            .field("domain_max", &self.data.domain_max)
            .field("entries", &self.data.table.len())
            .finish()
    }
}

struct LutData {
    id: u64,
    title: Option<String>,
    size: u32,
    is_3d: bool,
    domain_min: [f32; 3],
    domain_max: [f32; 3],
    /// 3D: `size^3` entries, index `r + g * size + b * size * size`. 1D: `size` entries.
    table: Vec<[f32; 3]>,
    /// The `rgba16float` upload of a 3D table, built on first use.
    texels: OnceLock<Arc<Vec<u16>>>,
}

/// Errors from [`parse_cube`] and [`load_cube`].
#[derive(Debug, Error)]
pub enum LutError {
    /// The file could not be read.
    #[error("cannot read the LUT file {path}: {message}")]
    Read {
        /// The path that failed.
        path: String,
        /// The underlying error message.
        message: String,
    },
    /// The file is larger than [`MAX_LUT_FILE_BYTES`].
    #[error("the LUT file is {size} bytes, over the {limit}-byte limit")]
    TooLarge {
        /// Size of the file in bytes. For a file that grew while it was read, one more than the limit.
        size: u64,
        /// The limit in bytes.
        limit: u64,
    },
    /// A line could not be parsed or breaks a rule that applies to that line.
    #[error("line {line}: {message}")]
    Syntax {
        /// 1-based line number.
        line: usize,
        /// What is wrong with the line.
        message: String,
    },
    /// The file as a whole is not a valid table (missing size, wrong row count, bad domain).
    #[error("invalid LUT: {0}")]
    Invalid(String),
}

impl Lut {
    /// The `TITLE` of the table, if the file has one.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.data.title.as_deref()
    }

    /// The edge length: `LUT_3D_SIZE` for a 3D table, `LUT_1D_SIZE` for a 1D table.
    #[must_use]
    pub fn size(&self) -> u32 {
        self.data.size
    }

    /// True for a 3D table (`LUT_3D_SIZE`).
    #[must_use]
    pub fn is_3d(&self) -> bool {
        self.data.is_3d
    }

    /// The per-channel input minimum (`DOMAIN_MIN`, default 0).
    #[must_use]
    pub fn domain_min(&self) -> [f32; 3] {
        self.data.domain_min
    }

    /// The per-channel input maximum (`DOMAIN_MAX`, default 1).
    #[must_use]
    pub fn domain_max(&self) -> [f32; 3] {
        self.data.domain_max
    }

    /// Identity of this parse. Clones share it; a new parse gets a new one.
    #[must_use]
    pub(crate) fn id(&self) -> u64 {
        self.data.id
    }

    /// Applies the table to one encoded RGB value on the CPU, in `f32`.
    ///
    /// A 3D table uses tetrahedral interpolation, and a 1D table interpolates each channel on its own column. Inputs
    /// outside the domain clamp to it. This is the reference that the GPU path is tested against; the GPU reads the
    /// same table as `f16`, so the two can differ by about one part in a thousand.
    #[must_use]
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        if self.data.is_3d {
            self.sample_3d(rgb)
        } else {
            self.sample_1d(rgb)
        }
    }

    /// The `rgba16float` texels of a 3D table, in texture order (red fastest), with alpha 1. Built once per table.
    pub(crate) fn rgba16f(&self) -> Arc<Vec<u16>> {
        Arc::clone(self.data.texels.get_or_init(|| {
            let mut texels = Vec::with_capacity(self.data.table.len() * 4);
            for rgb in &self.data.table {
                texels.extend_from_slice(&[
                    f32_to_f16_bits(rgb[0]),
                    f32_to_f16_bits(rgb[1]),
                    f32_to_f16_bits(rgb[2]),
                    F16_ONE,
                ]);
            }
            Arc::new(texels)
        }))
    }

    /// Maps one input channel into table coordinates in `[0, 1]`.
    fn domain_position(&self, value: f32, channel: usize) -> f32 {
        let lo = self.data.domain_min[channel];
        let hi = self.data.domain_max[channel];
        if !value.is_finite() {
            return 0.0;
        }
        ((value - lo) / (hi - lo)).clamp(0.0, 1.0)
    }

    fn sample_1d(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.data.size as usize;
        let mut out = [0.0_f32; 3];
        for (channel, out_value) in out.iter_mut().enumerate() {
            let x = self.domain_position(rgb[channel], channel) * (n - 1) as f32;
            let base = (x.floor() as usize).min(n - 2);
            let f = x - base as f32;
            let a = self.data.table[base][channel];
            let b = self.data.table[base + 1][channel];
            *out_value = a + (b - a) * f;
        }
        out
    }

    fn sample_3d(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.data.size as usize;
        let scale = (n - 1) as f32;
        let mut base = [0_usize; 3];
        let mut f = [0.0_f32; 3];
        for channel in 0..3 {
            let x = self.domain_position(rgb[channel], channel) * scale;
            base[channel] = (x.floor() as usize).min(n - 2);
            f[channel] = x - base[channel] as f32;
        }
        let at = |dr: usize, dg: usize, db: usize| {
            self.data.table[(base[0] + dr) + (base[1] + dg) * n + (base[2] + db) * n * n]
        };
        let c000 = at(0, 0, 0);
        let c100 = at(1, 0, 0);
        let c010 = at(0, 1, 0);
        let c001 = at(0, 0, 1);
        let c110 = at(1, 1, 0);
        let c101 = at(1, 0, 1);
        let c011 = at(0, 1, 1);
        let c111 = at(1, 1, 1);
        let (fr, fg, fb) = (f[0], f[1], f[2]);
        // Each branch walks the cube diagonal in the order of the fractions, as in the WGSL shader.
        let walk = |steps: [([f32; 3], f32); 3], origin: [f32; 3]| {
            let mut out = origin;
            for (delta, weight) in steps {
                for c in 0..3 {
                    out[c] += weight * delta[c];
                }
            }
            out
        };
        let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        if fr >= fg {
            if fg >= fb {
                walk(
                    [
                        (sub(c100, c000), fr),
                        (sub(c110, c100), fg),
                        (sub(c111, c110), fb),
                    ],
                    c000,
                )
            } else if fr >= fb {
                walk(
                    [
                        (sub(c100, c000), fr),
                        (sub(c101, c100), fb),
                        (sub(c111, c101), fg),
                    ],
                    c000,
                )
            } else {
                walk(
                    [
                        (sub(c001, c000), fb),
                        (sub(c101, c001), fr),
                        (sub(c111, c101), fg),
                    ],
                    c000,
                )
            }
        } else if fb >= fg {
            walk(
                [
                    (sub(c001, c000), fb),
                    (sub(c011, c001), fg),
                    (sub(c111, c011), fr),
                ],
                c000,
            )
        } else if fb >= fr {
            walk(
                [
                    (sub(c010, c000), fg),
                    (sub(c011, c010), fb),
                    (sub(c111, c011), fr),
                ],
                c000,
            )
        } else {
            walk(
                [
                    (sub(c010, c000), fg),
                    (sub(c110, c010), fr),
                    (sub(c111, c110), fb),
                ],
                c000,
            )
        }
    }
}

/// Reads and parses a `.cube` file, one line at a time through a buffered reader.
///
/// # Errors
///
/// Returns [`LutError::Read`] when the file cannot be opened or read, or when it is not a regular file (a directory,
/// a device or a pipe). Returns [`LutError::TooLarge`] when it exceeds [`MAX_LUT_FILE_BYTES`], including a file that
/// grew after its size was checked. Otherwise returns the parse errors of [`parse_cube`].
pub fn load_cube(path: &Path) -> Result<Lut, LutError> {
    let read_error = |error: std::io::Error| LutError::Read {
        path: path.display().to_string(),
        message: error.to_string(),
    };
    let metadata = std::fs::metadata(path).map_err(read_error)?;
    if !metadata.is_file() {
        return Err(LutError::Read {
            path: path.display().to_string(),
            message: "not a regular file".to_owned(),
        });
    }
    if metadata.len() > MAX_LUT_FILE_BYTES {
        return Err(LutError::TooLarge {
            size: metadata.len(),
            limit: MAX_LUT_FILE_BYTES,
        });
    }
    let file = std::fs::File::open(path).map_err(read_error)?;
    // The reader stops one byte past the limit. A file that grows after the check above then ends with the limit used
    // up, which is caught below.
    let mut reader = BufReader::new(file.take(MAX_LUT_FILE_BYTES + 1));
    let mut parser = CubeParser::new();
    for (index, line) in reader.by_ref().lines().enumerate() {
        let line = line.map_err(read_error)?;
        parser.feed(&line, index + 1)?;
    }
    if reader.get_ref().limit() == 0 {
        return Err(LutError::TooLarge {
            size: MAX_LUT_FILE_BYTES + 1,
            limit: MAX_LUT_FILE_BYTES,
        });
    }
    parser.finish()
}

/// Parses the text of a `.cube` file.
///
/// # Errors
///
/// Returns [`LutError::Syntax`] with the line number for a bad line, and [`LutError::Invalid`] for a file-level
/// problem: no size keyword, a size out of range, a wrong row count or a domain whose minimum is not below its
/// maximum.
pub fn parse_cube(text: &str) -> Result<Lut, LutError> {
    let mut parser = CubeParser::new();
    for (index, line) in text.lines().enumerate() {
        parser.feed(line, index + 1)?;
    }
    parser.finish()
}

/// Line-by-line state of a `.cube` parse. [`parse_cube`] and [`load_cube`] both feed it, so they accept the same syntax.
struct CubeParser {
    title: Option<String>,
    /// `(is_3d, size)` once the size keyword has been read.
    size_kind: Option<(bool, u32)>,
    domain_min: [f32; 3],
    domain_max: [f32; 3],
    table: Vec<[f32; 3]>,
    data_started: bool,
}

impl CubeParser {
    fn new() -> Self {
        Self {
            title: None,
            size_kind: None,
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
            table: Vec::new(),
            data_started: false,
        }
    }

    /// Reads one line. `line` is its 1-based number, used in errors.
    fn feed(&mut self, raw: &str, line: usize) -> Result<(), LutError> {
        let raw = if line == 1 {
            raw.strip_prefix('\u{feff}').unwrap_or(raw)
        } else {
            raw
        };
        let trimmed = raw.trim();
        let syntax = |message: String| LutError::Syntax { line, message };

        // A TITLE line takes the rest of the line as its value, so a '#' inside the value is part of the title.
        let head = trimmed
            .split(|c: char| c.is_whitespace() || c == '#')
            .next()
            .unwrap_or("");
        if head.eq_ignore_ascii_case("TITLE") {
            if self.data_started {
                return Err(syntax(format!(
                    "keyword {head} appears after the data rows"
                )));
            }
            let value = trimmed[head.len()..].trim();
            let unquoted = value
                .strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .unwrap_or(value);
            self.title = (!unquoted.is_empty()).then(|| unquoted.to_owned());
            return Ok(());
        }

        let content = match trimmed.find('#') {
            Some(position) => &trimmed[..position],
            None => trimmed,
        }
        .trim();
        let mut words = content.split_whitespace();
        let Some(first) = words.next() else {
            return Ok(());
        };

        if first.starts_with(|c: char| c.is_ascii_alphabetic()) {
            if self.data_started {
                return Err(syntax(format!(
                    "keyword {first} appears after the data rows"
                )));
            }
            let keyword = first.to_ascii_uppercase();
            let rest: Vec<&str> = words.collect();
            match keyword.as_str() {
                "DOMAIN_MIN" => self.domain_min = parse_triple(&rest, line)?,
                "DOMAIN_MAX" => self.domain_max = parse_triple(&rest, line)?,
                "LUT_1D_INPUT_RANGE" | "LUT_3D_INPUT_RANGE" => {
                    let [lo, hi] = parse_pair(&rest, line)?;
                    self.domain_min = [lo; 3];
                    self.domain_max = [hi; 3];
                }
                "LUT_1D_SIZE" | "LUT_3D_SIZE" => {
                    let is_3d = keyword == "LUT_3D_SIZE";
                    if self.size_kind.is_some() {
                        return Err(syntax(
                            "the LUT_1D_SIZE or LUT_3D_SIZE keyword appears twice".to_owned(),
                        ));
                    }
                    let size = parse_size(&rest, is_3d, line)?;
                    self.size_kind = Some((is_3d, size));
                }
                other => return Err(syntax(format!("unknown keyword {other}"))),
            }
            return Ok(());
        }

        // A data row: exactly three finite values.
        let Some((is_3d, size)) = self.size_kind else {
            return Err(syntax(
                "a data row appears before LUT_1D_SIZE or LUT_3D_SIZE".to_owned(),
            ));
        };
        let values: Vec<&str> = std::iter::once(first).chain(words).collect();
        let row = parse_triple(&values, line)?;
        let expected = expected_rows(is_3d, size);
        if self.table.len() >= expected {
            return Err(syntax(format!(
                "more data rows than the {expected} the table size allows"
            )));
        }
        self.table.push(row);
        self.data_started = true;
        Ok(())
    }

    /// Checks the file-level rules and builds the table.
    fn finish(self) -> Result<Lut, LutError> {
        let Some((is_3d, size)) = self.size_kind else {
            return Err(LutError::Invalid(
                "the file has no LUT_1D_SIZE or LUT_3D_SIZE keyword".to_owned(),
            ));
        };
        let expected = expected_rows(is_3d, size);
        if self.table.len() != expected {
            return Err(LutError::Invalid(format!(
                "a {size}-entry {} table needs {expected} data rows, found {}",
                if is_3d { "3D" } else { "1D" },
                self.table.len()
            )));
        }
        if (0..3).any(|c| self.domain_min[c] >= self.domain_max[c]) {
            return Err(LutError::Invalid(
                "DOMAIN_MIN must be below DOMAIN_MAX in every channel".to_owned(),
            ));
        }
        Ok(Lut {
            data: Arc::new(LutData {
                id: NEXT_LUT_ID.fetch_add(1, Ordering::Relaxed),
                title: self.title,
                size,
                is_3d,
                domain_min: self.domain_min,
                domain_max: self.domain_max,
                table: self.table,
                texels: OnceLock::new(),
            }),
        })
    }
}

/// Number of data rows a table of this kind and size must contain.
fn expected_rows(is_3d: bool, size: u32) -> usize {
    let n = size as usize;
    if is_3d { n * n * n } else { n }
}

/// Parses `LUT_*_SIZE n` and checks the range for its kind.
fn parse_size(words: &[&str], is_3d: bool, line: usize) -> Result<u32, LutError> {
    let [word] = words else {
        return Err(LutError::Syntax {
            line,
            message: "the size keyword takes one integer".to_owned(),
        });
    };
    let size: u32 = word.parse().map_err(|_| LutError::Syntax {
        line,
        message: format!("{word} is not a whole number"),
    })?;
    let (min, max) = if is_3d {
        (2, MAX_3D_SIZE)
    } else {
        (2, MAX_1D_SIZE)
    };
    if !(min..=max).contains(&size) {
        return Err(LutError::Syntax {
            line,
            message: format!(
                "{} must be {min} to {max}, found {size}",
                if is_3d { "LUT_3D_SIZE" } else { "LUT_1D_SIZE" }
            ),
        });
    }
    Ok(size)
}

/// Parses exactly three finite floats.
fn parse_triple(words: &[&str], line: usize) -> Result<[f32; 3], LutError> {
    if words.len() != 3 {
        return Err(LutError::Syntax {
            line,
            message: format!("expected three values, found {}", words.len()),
        });
    }
    Ok([
        parse_finite(words[0], line)?,
        parse_finite(words[1], line)?,
        parse_finite(words[2], line)?,
    ])
}

/// Parses exactly two finite floats.
fn parse_pair(words: &[&str], line: usize) -> Result<[f32; 2], LutError> {
    if words.len() != 2 {
        return Err(LutError::Syntax {
            line,
            message: format!("expected two values, found {}", words.len()),
        });
    }
    Ok([parse_finite(words[0], line)?, parse_finite(words[1], line)?])
}

fn parse_finite(word: &str, line: usize) -> Result<f32, LutError> {
    let value: f32 = word.parse().map_err(|_| LutError::Syntax {
        line,
        message: format!("{word} is not a number"),
    })?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(LutError::Syntax {
            line,
            message: format!("{word} is not finite"),
        })
    }
}

/// IEEE 754 binary16 bit pattern of 1.0.
const F16_ONE: u16 = 0x3C00;

/// Converts an `f32` to the nearest IEEE 754 binary16 bit pattern (ties to even). Values beyond the half range
/// become infinity, and NaN keeps a payload bit. The table values are finite, so NaN only guards the input.
pub(crate) fn f32_to_f16_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xFF) as i32;
    let mantissa = bits & 0x007F_FFFF;
    if exponent == 0xFF {
        return sign | 0x7C00 | if mantissa != 0 { 0x0200 } else { 0 };
    }
    // Re-bias from f32 (127) to f16 (15).
    let half_exponent = exponent - 127 + 15;
    if half_exponent >= 0x1F {
        return sign | 0x7C00;
    }
    if half_exponent <= 0 {
        // Subnormal in binary16 (or flushed to zero): shift the full significand into place.
        if half_exponent < -10 {
            return sign;
        }
        let significand = mantissa | 0x0080_0000;
        let shift = (14 - half_exponent) as u32;
        let mut half = significand >> shift;
        let remainder = significand & ((1 << shift) - 1);
        let halfway = 1 << (shift - 1);
        if remainder > halfway || (remainder == halfway && half & 1 == 1) {
            half += 1;
        }
        return sign | half as u16;
    }
    let mut half = ((half_exponent as u32) << 10) | (mantissa >> 13);
    let remainder = mantissa & 0x1FFF;
    if remainder > 0x1000 || (remainder == 0x1000 && half & 1 == 1) {
        // A carry out of the mantissa correctly bumps the exponent, up to infinity.
        half += 1;
    }
    sign | half as u16
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// splitmix64, so the random tables and points are the same on every run.
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        /// A value in `[0, 1)`.
        fn unit(&mut self) -> f32 {
            (self.next_u64() >> 40) as f32 / (1_u64 << 24) as f32
        }
    }

    /// Tetrahedral interpolation in `f64`, written from the barycentric form rather than the branch structure of
    /// `sample_3d`. The main diagonal splits each cell into six tetrahedra. Sorting the fractions picks the tetrahedron:
    /// for `f0 >= f1 >= f2` its corners are the base, the base plus one step along the first axis, then along the
    /// second, and the far corner, with weights `1 - f0`, `f0 - f1`, `f1 - f2` and `f2`.
    fn reference_tetrahedral(lut: &Lut, rgb: [f32; 3]) -> [f64; 3] {
        let n = lut.data.size as usize;
        let mut base = [0_usize; 3];
        let mut frac = [0.0_f64; 3];
        for c in 0..3 {
            let lo = f64::from(lut.data.domain_min[c]);
            let hi = f64::from(lut.data.domain_max[c]);
            let t = ((f64::from(rgb[c]) - lo) / (hi - lo)).clamp(0.0, 1.0) * (n - 1) as f64;
            base[c] = (t.floor() as usize).min(n - 2);
            frac[c] = t - base[c] as f64;
        }
        let mut order = [0_usize, 1, 2];
        order.sort_by(|&a, &b| frac[b].total_cmp(&frac[a]));
        let weights = [
            1.0 - frac[order[0]],
            frac[order[0]] - frac[order[1]],
            frac[order[1]] - frac[order[2]],
            frac[order[2]],
        ];
        let steps = [None, Some(order[0]), Some(order[1]), Some(order[2])];
        let mut corner = base;
        let mut out = [0.0_f64; 3];
        for (weight, step) in weights.into_iter().zip(steps) {
            if let Some(axis) = step {
                corner[axis] += 1;
            }
            let value = lut.data.table[corner[0] + corner[1] * n + corner[2] * n * n];
            for c in 0..3 {
                out[c] += weight * f64::from(value[c]);
            }
        }
        out
    }

    /// A 3D `.cube` text with random entries (seeded) and a non-default domain, so the domain mapping is exercised.
    fn random_cube_text(seed: u64, size: usize) -> String {
        let mut rng = Rng(seed);
        let mut text =
            format!("DOMAIN_MIN 0.1 0.2 0.0\nDOMAIN_MAX 0.9 0.8 1.0\nLUT_3D_SIZE {size}\n");
        for _ in 0..size * size * size {
            let (r, g, b) = (rng.unit(), rng.unit(), rng.unit());
            writeln!(text, "{r:.7} {g:.7} {b:.7}").expect("write to string");
        }
        text
    }

    #[test]
    fn reference_is_the_identity_on_an_identity_table() {
        let mut text = String::from("LUT_3D_SIZE 9\n");
        for b in 0..9 {
            for g in 0..9 {
                for r in 0..9 {
                    writeln!(
                        text,
                        "{} {} {}",
                        r as f32 / 8.0,
                        g as f32 / 8.0,
                        b as f32 / 8.0
                    )
                    .expect("write to string");
                }
            }
        }
        let lut = parse_cube(&text).expect("parse");
        let mut rng = Rng(7);
        for _ in 0..500 {
            let rgb = [rng.unit(), rng.unit(), rng.unit()];
            let out = reference_tetrahedral(&lut, rgb);
            for c in 0..3 {
                assert!(
                    (out[c] - f64::from(rgb[c])).abs() < 1e-6,
                    "{rgb:?} -> {out:?}"
                );
            }
        }
    }

    #[test]
    fn sample_3d_matches_the_barycentric_reference() {
        for (seed, size) in [(1_u64, 2_usize), (2, 3), (3, 9), (4, 17)] {
            let lut = parse_cube(&random_cube_text(seed, size)).expect("parse");
            let mut rng = Rng(seed ^ 0xABCD);
            let mut points: Vec<[f32; 3]> = (0..4000)
                .map(|_| [rng.unit(), rng.unit(), rng.unit()])
                .collect();
            // Corners, a domain edge, inputs outside the domain and the centre.
            points.extend([
                [0.0; 3],
                [1.0; 3],
                [0.1, 0.2, 0.0],
                [0.9, 0.8, 1.0],
                [-0.5, 2.0, 0.5],
                [0.5; 3],
            ]);
            // Exact ties between the fractions: the main diagonal and the two face diagonals.
            for k in 0..=8 {
                let v = k as f32 / 8.0;
                points.push([v, v, v]);
                points.push([v, v, 0.0]);
                points.push([0.0, v, v]);
            }
            for rgb in points {
                let fast = lut.sample_3d(rgb);
                let reference = reference_tetrahedral(&lut, rgb);
                for c in 0..3 {
                    assert!(
                        (f64::from(fast[c]) - reference[c]).abs() < 1e-5,
                        "size {size}, {rgb:?}: {fast:?} vs {reference:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn debug_shows_the_header_not_the_table() {
        let lut = parse_cube(&random_cube_text(9, 17)).expect("parse");
        let text = format!("{lut:?}");
        assert!(text.contains("entries: 4913"), "{text}");
        assert!(text.len() < 400, "debug output is {} bytes", text.len());
    }

    #[test]
    fn f16_known_values() {
        assert_eq!(f32_to_f16_bits(0.0), 0x0000);
        assert_eq!(f32_to_f16_bits(-0.0), 0x8000);
        assert_eq!(f32_to_f16_bits(1.0), 0x3C00);
        assert_eq!(f32_to_f16_bits(-1.0), 0xBC00);
        assert_eq!(f32_to_f16_bits(0.5), 0x3800);
        assert_eq!(f32_to_f16_bits(2.0), 0x4000);
        assert_eq!(f32_to_f16_bits(65_504.0), 0x7BFF);
        assert_eq!(f32_to_f16_bits(65_520.0), 0x7C00);
        assert_eq!(f32_to_f16_bits(f32::INFINITY), 0x7C00);
        // Smallest positive subnormal binary16 is 2^-24.
        assert_eq!(f32_to_f16_bits(2f32.powi(-24)), 0x0001);
        assert_eq!(f32_to_f16_bits(2f32.powi(-26)), 0x0000);
    }

    #[test]
    fn f16_is_close_on_unit_range() {
        for i in 0..=1000 {
            let x = i as f32 / 1000.0;
            let bits = f32_to_f16_bits(x);
            let back = f16_to_f32(bits);
            assert!(
                (back - x).abs() <= x.max(1e-4) * 0.000_5 + 1e-7,
                "{x} -> {back}"
            );
        }
    }

    /// Reference decode, used only to check the encoder.
    fn f16_to_f32(bits: u16) -> f32 {
        let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
        let exponent = i32::from((bits >> 10) & 0x1F);
        let mantissa = f32::from(bits & 0x03FF);
        if exponent == 0 {
            sign * mantissa * 2f32.powi(-24)
        } else {
            sign * (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15)
        }
    }
}
