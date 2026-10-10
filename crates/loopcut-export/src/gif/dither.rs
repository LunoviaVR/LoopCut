//! Ordered dithering for the GPU-dither engine, in integer arithmetic.
//!
//! A pixel is dithered by adding a threshold-dependent offset to each channel, then taking the nearest palette entry
//! by squared RGB distance. Every step is integer arithmetic with a fixed tie-break (the lowest palette index wins), so
//! the WGSL pass in [`crate::gif::gpu_dither`] and [`map_frame`] produce the same indices bit for bit.
//!
//! The threshold matrices are an 8x8 Bayer matrix and a 16x16 blue-noise matrix made by void-and-cluster. The
//! blue-noise matrix is generated once, deterministically, on first use.

use std::collections::HashMap;
use std::sync::OnceLock;

use loopcut_core::project::Dither;

/// Side of the Bayer matrix.
pub const BAYER_SIZE: u32 = 8;
/// Side of the blue-noise matrix.
pub const BLUE_NOISE_SIZE: u32 = 16;
/// Cells the GPU threshold buffer holds. Both matrices fit.
pub const MATRIX_CAPACITY: usize = 256;

/// Dithering parameters for one palette. `amplitude` is the full offset range in 0..=255 units, so the offset of a
/// cell lies in `[-amplitude / 2, amplitude / 2)`. An amplitude of 0 is nearest-colour mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DitherSpec {
    /// Side of the threshold matrix: [`BAYER_SIZE`] or [`BLUE_NOISE_SIZE`].
    pub matrix_size: u32,
    /// Thresholds in row-major order, `matrix_size` squared entries, each in 0..=255.
    pub thresholds: Vec<u8>,
    /// Offset range in 0..=255 units. Zero disables dithering.
    pub amplitude: u32,
}

impl DitherSpec {
    /// Nearest-colour mapping: no offset.
    pub fn nearest() -> Self {
        Self {
            matrix_size: BAYER_SIZE,
            thresholds: bayer_thresholds(),
            amplitude: 0,
        }
    }

    /// The thresholds as `MATRIX_CAPACITY` entries, zero-padded, for the GPU buffer.
    pub fn padded_thresholds(&self) -> [u32; MATRIX_CAPACITY] {
        let mut out = [0u32; MATRIX_CAPACITY];
        for (slot, &value) in out.iter_mut().zip(self.thresholds.iter()) {
            *slot = u32::from(value);
        }
        out
    }
}

/// The dithering spec for a project's dither setting and palette size.
///
/// None maps to nearest colour. Bayer uses the Bayer matrix at full strength. Blue noise uses the blue-noise matrix at
/// 80 % strength. Floyd-Steinberg and Sierra error diffusion cannot run as an ordered pass, so both map onto the Bayer
/// matrix at 60 % strength. The strengths are tuning constants.
pub fn spec_for(dither: Dither, palette_len: usize) -> DitherSpec {
    let (size, thresholds, strength) = match dither {
        Dither::None => return DitherSpec::nearest(),
        Dither::Bayer => (BAYER_SIZE, bayer_thresholds(), 1.0),
        Dither::FloydSteinberg | Dither::Sierra2_4a => (BAYER_SIZE, bayer_thresholds(), 0.6),
        Dither::BlueNoise => (BLUE_NOISE_SIZE, blue_noise_thresholds(), 0.8),
    };
    DitherSpec {
        matrix_size: size,
        thresholds,
        amplitude: amplitude_for(strength, palette_len),
    }
}

/// The offset range for `strength` on a palette of `palette_len` entries. A palette of `n` entries spans about the cube
/// root of `n` levels per channel, so one step is `255 / cbrt(n)`. The range is `strength` steps, which dithers across
/// half a step either side of the nearest colour.
pub fn amplitude_for(strength: f64, palette_len: usize) -> u32 {
    let levels = (palette_len.max(1) as f64).cbrt();
    (strength * 255.0 / levels).round().clamp(0.0, 255.0) as u32
}

/// Bayer thresholds for the 8x8 matrix, spread over 2..=254.
fn bayer_thresholds() -> Vec<u8> {
    let size = BAYER_SIZE as usize;
    let mut out = Vec::with_capacity(size * size);
    for y in 0..size {
        for x in 0..size {
            let rank = bayer_rank(x as u32, y as u32, 3);
            out.push((rank * 4 + 2) as u8);
        }
    }
    out
}

/// The Bayer rank of cell `(x, y)` in a matrix of side `2^bits`, from the bit-interleaved Gray-code construction.
fn bayer_rank(x: u32, y: u32, bits: u32) -> u32 {
    let mut rank = 0;
    for bit in 0..bits {
        let xi = (x >> bit) & 1;
        let yi = (y >> bit) & 1;
        rank |= ((xi ^ yi) << (2 * bit + 1)) | (yi << (2 * bit));
    }
    rank
}

/// Blue-noise thresholds for the 16x16 matrix, one per cell.
fn blue_noise_thresholds() -> Vec<u8> {
    static CACHE: OnceLock<Vec<u8>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            void_and_cluster(BLUE_NOISE_SIZE as usize)
                .into_iter()
                .map(|rank| rank as u8)
                .collect()
        })
        .clone()
}

/// Ranks for a `side x side` torus by void-and-cluster (Ulichney 1993).
///
/// Ranks are a permutation of `0..side*side`. Low ranks are the largest voids and high ranks the tightest clusters, so a
/// threshold of `rank` lights cells evenly at every density. Every choice breaks ties on the lowest index, and the
/// starting pattern comes from a fixed-seed generator, so the result is the same on every run.
pub fn void_and_cluster(side: usize) -> Vec<u32> {
    let cells = side * side;
    let sigma = 1.5_f64;
    let mut kernel = vec![0.0_f64; cells * cells];
    for i in 0..cells {
        let (ix, iy) = ((i % side) as i64, (i / side) as i64);
        for j in 0..cells {
            let (jx, jy) = ((j % side) as i64, (j / side) as i64);
            let dx = torus_distance(ix - jx, side as i64);
            let dy = torus_distance(iy - jy, side as i64);
            kernel[i * cells + j] = (-(dx * dx + dy * dy) / (2.0 * sigma * sigma)).exp();
        }
    }

    let ones = ((cells as f64) * 0.1).round().max(1.0) as usize;
    let initial = initial_pattern(cells, ones);
    let set = relax(&kernel, cells, initial);

    let mut ranks = vec![0u32; cells];
    // Phase 1: remove the tightest cluster repeatedly. The removed cells take the highest ranks of the initial set.
    let mut members = set.clone();
    let mut energy = energies(&kernel, cells, &members);
    let mut count = ones;
    while count > 0 {
        let cell = argmax_where(&energy, &members, true);
        members[cell] = false;
        subtract_row(&mut energy, &kernel, cells, cell);
        count -= 1;
        ranks[cell] = count as u32;
    }
    // Phase 2: starting from the initial set, add cells to the largest void. They take ranks from `ones` upward.
    let mut members = set;
    let mut energy = energies(&kernel, cells, &members);
    let mut count = ones;
    while count < cells {
        let cell = min_index_where(&energy, &members);
        members[cell] = true;
        add_row(&mut energy, &kernel, cells, cell);
        ranks[cell] = count as u32;
        count += 1;
    }
    ranks
}

/// The shortest distance between two coordinates on a torus of length `period`.
fn torus_distance(delta: i64, period: i64) -> f64 {
    let d = delta.rem_euclid(period);
    d.min(period - d) as f64
}

/// A deterministic initial pattern with `ones` cells set, chosen by a fixed-seed Fisher-Yates shuffle.
fn initial_pattern(cells: usize, ones: usize) -> Vec<bool> {
    let mut order: Vec<usize> = (0..cells).collect();
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    for i in (1..cells).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let j = (state % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
    let mut set = vec![false; cells];
    for &cell in order.iter().take(ones) {
        set[cell] = true;
    }
    set
}

/// Swaps the tightest member with the largest void while that lowers the energy gap. Bounded so it always ends.
fn relax(kernel: &[f64], cells: usize, mut set: Vec<bool>) -> Vec<bool> {
    for _ in 0..cells * 8 {
        let energy = energies(kernel, cells, &set);
        let tight = argmax_where(&energy, &set, true);
        let void = min_index_where(&energy, &set);
        if energy[tight] <= energy[void] {
            break;
        }
        set[tight] = false;
        set[void] = true;
    }
    set
}

/// Energy of every cell: the sum of kernel contributions from the members.
fn energies(kernel: &[f64], cells: usize, members: &[bool]) -> Vec<f64> {
    let mut energy = vec![0.0; cells];
    for (q, &member) in members.iter().enumerate() {
        if member {
            add_row(&mut energy, kernel, cells, q);
        }
    }
    energy
}

fn add_row(energy: &mut [f64], kernel: &[f64], cells: usize, cell: usize) {
    let row = &kernel[cell * cells..(cell + 1) * cells];
    for (e, k) in energy.iter_mut().zip(row) {
        *e += k;
    }
}

fn subtract_row(energy: &mut [f64], kernel: &[f64], cells: usize, cell: usize) {
    let row = &kernel[cell * cells..(cell + 1) * cells];
    for (e, k) in energy.iter_mut().zip(row) {
        *e -= k;
    }
}

/// The lowest-index cell with the largest energy among cells whose membership is `member`.
fn argmax_where(energy: &[f64], members: &[bool], member: bool) -> usize {
    let mut best: Option<usize> = None;
    for (i, (&e, &m)) in energy.iter().zip(members).enumerate() {
        if m == member && best.is_none_or(|b| e > energy[b]) {
            best = Some(i);
        }
    }
    best.unwrap_or(0)
}

/// The lowest-index non-member with the smallest energy, which is the largest void.
fn min_index_where(energy: &[f64], members: &[bool]) -> usize {
    let mut best: Option<usize> = None;
    for (i, (&e, &m)) in energy.iter().zip(members).enumerate() {
        if !m && best.is_none_or(|b| e < energy[b]) {
            best = Some(i);
        }
    }
    best.unwrap_or(0)
}

/// The colour one pixel is matched against: its channels plus the threshold offset of its cell, clamped to 0..=255.
///
/// `x` and `y` index the threshold matrix modulo its size. With a zero amplitude the colour is not offset.
pub fn dithered_color(rgb: [u8; 3], x: u32, y: u32, spec: &DitherSpec) -> [u8; 3] {
    let size = spec.matrix_size.max(1);
    let threshold = match spec
        .thresholds
        .get(((y % size) * size + (x % size)) as usize)
    {
        Some(&value) => i32::from(value),
        None => 0,
    };
    let amplitude = spec.amplitude as i32;
    let offset = ((threshold * amplitude) >> 8) - (amplitude >> 1);
    rgb.map(|channel| (i32::from(channel) + offset).clamp(0, 255) as u8)
}

/// The palette index for one pixel: the nearest palette entry to the dithered colour.
pub fn map_pixel(rgb: [u8; 3], x: u32, y: u32, palette: &[[u8; 3]], spec: &DitherSpec) -> u8 {
    nearest(dithered_color(rgb, x, y, spec).map(i32::from), palette)
}

/// The lowest-index palette entry with the smallest squared RGB distance. An empty palette maps to index 0.
pub fn nearest(color: [i32; 3], palette: &[[u8; 3]]) -> u8 {
    let mut best = 0usize;
    let mut best_distance = i32::MAX;
    for (index, entry) in palette.iter().enumerate() {
        let dr = color[0] - i32::from(entry[0]);
        let dg = color[1] - i32::from(entry[1]);
        let db = color[2] - i32::from(entry[2]);
        let distance = dr * dr + dg * dg + db * db;
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    best as u8
}

/// Maps a tightly packed RGBA8 frame onto the palette, one index per pixel, row-major. `out` is refilled.
///
/// This is the CPU path, and the reference the GPU pass must match. Each distinct dithered colour is matched once per
/// frame and its index is reused for every pixel with that colour, which is the same index [`nearest`] gives, ties
/// included.
pub fn map_frame(
    rgba: &[u8],
    width: u32,
    height: u32,
    palette: &[[u8; 3]],
    spec: &DitherSpec,
    out: &mut Vec<u8>,
) {
    out.clear();
    debug_assert_eq!(rgba.len(), (width as usize) * (height as usize) * 4);
    let row = width.max(1) as usize;
    // Keyed by the dithered colour, which is all the nearest-colour search depends on.
    let mut memo: HashMap<[u8; 3], u8> = HashMap::new();
    // Pixels come from `as_chunks`, so a short buffer gives fewer indices instead of a panic. The caller checks the length.
    out.extend(rgba.as_chunks::<4>().0.iter().enumerate().map(|(i, px)| {
        let x = (i % row) as u32;
        let y = (i / row) as u32;
        let color = dithered_color([px[0], px[1], px[2]], x, y, spec);
        *memo
            .entry(color)
            .or_insert_with(|| nearest(color.map(i32::from), palette))
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bayer_thresholds_are_a_permutation_of_ranks() {
        let mut values = bayer_thresholds();
        assert_eq!(values.len(), 64);
        values.sort_unstable();
        let expected: Vec<u8> = (0..64).map(|rank| (rank * 4 + 2) as u8).collect();
        assert_eq!(values, expected);
    }

    #[test]
    fn void_and_cluster_gives_a_permutation() {
        let ranks = void_and_cluster(BLUE_NOISE_SIZE as usize);
        let mut sorted = ranks.clone();
        sorted.sort_unstable();
        let expected: Vec<u32> = (0..256).collect();
        assert_eq!(sorted, expected);
    }

    #[test]
    fn blue_noise_spreads_the_lowest_ranks() {
        // The first 16 cells (about 6 % density) are never 4-neighbours on the torus: a clustered matrix would
        // put them side by side.
        let ranks = void_and_cluster(16);
        let mut cell_of_rank = vec![0usize; 256];
        for (cell, &rank) in ranks.iter().enumerate() {
            cell_of_rank[rank as usize] = cell;
        }
        let low: Vec<usize> = cell_of_rank[..16].to_vec();
        for &a in &low {
            for &b in &low {
                if a == b {
                    continue;
                }
                let (ax, ay) = (a % 16, a / 16);
                let (bx, by) = (b % 16, b / 16);
                let horizontal = ay == by && torus_distance(ax as i64 - bx as i64, 16) == 1.0;
                let vertical = ax == bx && torus_distance(ay as i64 - by as i64, 16) == 1.0;
                let adjacent = horizontal || vertical;
                assert!(!adjacent, "low-rank cells {a} and {b} are neighbours");
            }
        }
    }

    #[test]
    fn zero_amplitude_is_nearest_colour() {
        let palette = [[0, 0, 0], [255, 255, 255], [200, 30, 30]];
        let spec = DitherSpec::nearest();
        for (x, y) in [(0, 0), (3, 5), (7, 7)] {
            assert_eq!(map_pixel([250, 240, 250], x, y, &palette, &spec), 1);
            assert_eq!(map_pixel([10, 5, 0], x, y, &palette, &spec), 0);
        }
    }

    #[test]
    fn nearest_breaks_ties_on_the_lowest_index() {
        let palette = [[10, 10, 10], [10, 10, 10]];
        assert_eq!(nearest([10, 10, 10], &palette), 0);
        assert_eq!(nearest([0, 0, 0], &[]), 0);
    }

    #[test]
    fn dithering_moves_a_grey_between_two_palette_entries() {
        // A grey halfway between black and white becomes a mix of both across the matrix.
        let palette = [[0, 0, 0], [255, 255, 255]];
        let spec = spec_for(Dither::Bayer, palette.len());
        assert!(spec.amplitude > 0);
        let mut hits = [0usize; 2];
        for y in 0..8 {
            for x in 0..8 {
                hits[usize::from(map_pixel([128, 128, 128], x, y, &palette, &spec))] += 1;
            }
        }
        assert!(hits[0] > 0 && hits[1] > 0, "{hits:?}");
    }

    #[test]
    fn amplitude_is_one_palette_step() {
        // 256 entries: about 6.35 levels per channel, so a full-strength step is about 40 units.
        assert_eq!(amplitude_for(1.0, 256), 40);
        assert_eq!(amplitude_for(0.0, 256), 0);
        assert_eq!(amplitude_for(1.0, 1), 255);
    }

    #[test]
    fn spec_for_maps_each_setting() {
        assert_eq!(spec_for(Dither::None, 16).amplitude, 0);
        assert_eq!(spec_for(Dither::Bayer, 16).matrix_size, BAYER_SIZE);
        assert_eq!(spec_for(Dither::BlueNoise, 16).matrix_size, BLUE_NOISE_SIZE);
        assert!(
            spec_for(Dither::FloydSteinberg, 16).amplitude < spec_for(Dither::Bayer, 16).amplitude
        );
    }

    #[test]
    fn map_frame_is_deterministic() {
        let palette = [[0, 0, 0], [90, 90, 90], [180, 180, 180], [255, 255, 255]];
        let spec = spec_for(Dither::BlueNoise, palette.len());
        let frame: Vec<u8> = (0..(20 * 13 * 4)).map(|i| (i * 37 % 256) as u8).collect();
        let mut first = Vec::new();
        let mut second = Vec::new();
        map_frame(&frame, 20, 13, &palette, &spec, &mut first);
        map_frame(&frame, 20, 13, &palette, &spec, &mut second);
        assert_eq!(first.len(), 20 * 13);
        assert_eq!(first, second);
    }

    #[test]
    fn memoized_frame_mapping_matches_the_per_pixel_mapping() {
        // A palette with a duplicate entry, so the tie-break is exercised, and a frame with few distinct colours, so the
        // memo is hit for most pixels.
        let palette = [
            [0, 0, 0],
            [255, 255, 255],
            [200, 30, 30],
            [200, 30, 30],
            [40, 40, 220],
        ];
        let (width, height) = (23u32, 17u32);
        let rgba: Vec<u8> = (0..width * height * 4)
            .map(|i| ((i * 37) % 7 * 40) as u8)
            .collect();
        for dither in [Dither::None, Dither::Bayer, Dither::BlueNoise] {
            let spec = spec_for(dither, palette.len());
            let mut out = Vec::new();
            map_frame(&rgba, width, height, &palette, &spec, &mut out);
            let expected: Vec<u8> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .map(|(i, px)| {
                    map_pixel(
                        [px[0], px[1], px[2]],
                        (i % width as usize) as u32,
                        (i / width as usize) as u32,
                        &palette,
                        &spec,
                    )
                })
                .collect();
            assert_eq!(out, expected, "{dither:?}");
        }
    }

    #[test]
    fn a_duplicate_palette_entry_keeps_the_lowest_index() {
        let palette = [[0, 0, 0], [255, 255, 255], [200, 30, 30], [200, 30, 30]];
        let spec = DitherSpec::nearest();
        let rgba: Vec<u8> = [200u8, 30, 30, 255].repeat(6);
        let mut out = Vec::new();
        map_frame(&rgba, 3, 2, &palette, &spec, &mut out);
        assert_eq!(out, vec![2; 6]);
    }
}
