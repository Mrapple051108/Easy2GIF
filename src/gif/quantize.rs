//! Colour quantisation: reduce true-colour frames to an indexed palette.
//!
//! Implements Wu's variance-minimising colour quantiser (a pruned median cut
//! that splits each box along the axis with the largest weighted variance),
//! which produces noticeably better results than a plain median cut at the
//! same palette size.

use crate::error::{Error, Result};
use std::sync::atomic::{AtomicBool, Ordering};

fn check_cancel(cancel: Option<&AtomicBool>) -> Result<()> {
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

/// A palette of up to 256 RGB entries.
#[derive(Debug, Clone, Default)]
pub struct Palette {
    pub colors: Vec<[u8; 3]>,
}

impl Palette {
    pub fn len(&self) -> usize {
        self.colors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.colors.is_empty()
    }

    /// Smallest GIF colour-table size that can hold this palette: 2, 4, 8, ... 256.
    pub fn table_bits(&self) -> u8 {
        let mut bits = 1u8;
        while (1usize << bits) < self.colors.len().max(2) && bits < 8 {
            bits += 1;
        }
        bits
    }

    /// Palette contents padded to the colour-table size, as flat RGB bytes.
    pub fn table_bytes(&self) -> Vec<u8> {
        let size = 1usize << self.table_bits();
        let mut out = vec![0u8; size * 3];
        for (i, c) in self.colors.iter().take(size).enumerate() {
            out[i * 3] = c[0];
            out[i * 3 + 1] = c[1];
            out[i * 3 + 2] = c[2];
        }
        out
    }

    /// A copy with one extra entry appended, reserved for the transparent
    /// colour of the inter-frame optimisation.
    ///
    /// The slot has to exist so that [`Palette::table_bits`] sizes the colour
    /// table over the transparent index too, which is what stops the index from
    /// falling outside the table. The colour in it is never selected: the
    /// lookup table is built from the *original* palette, whose indices all sit
    /// below the reserved slot.
    pub fn with_reserved_slot(&self) -> Palette {
        let mut colors = self.colors.clone();
        colors.push([0, 0, 0]);
        Palette { colors }
    }

    /// Index reserved by [`Palette::with_reserved_slot`].
    pub fn reserved_index(&self) -> u8 {
        self.colors.len().saturating_sub(1).min(255) as u8
    }
}

/// A per-channel lookup table mapping any RGB colour to a palette index.
///
/// The table is quantised to 6 bits per channel (64³ = 256 KiB). That is fine
/// enough that the table's own rounding contributes well under one level of
/// error per channel — a visible improvement over the usual 5-bit cube, whose
/// ±4 levels per channel show up as banding in smooth gradients.
pub struct ColorMap {
    /// 64*64*64 entries.
    lut: Vec<u8>,
    exact: std::collections::HashMap<[u8; 3], u8>,
    exact_cells: Vec<u64>,
}

impl ColorMap {
    const BITS: u32 = 6;
    const SIDE: usize = 1 << Self::BITS;

    #[inline]
    fn key(r: u8, g: u8, b: u8) -> usize {
        let s = Self::BITS;
        (((r as usize) >> (8 - s)) << (2 * s))
            | (((g as usize) >> (8 - s)) << s)
            | ((b as usize) >> (8 - s))
    }

    pub fn build(palette: &Palette) -> ColorMap {
        Self::build_with_threads(palette, 1)
    }

    /// Build the lookup cube, splitting the work across `threads` workers.
    ///
    /// Every one of the 64*64*64 cells is an independent nearest-colour search
    /// over the palette, so this is a straight data-parallel split of the flat
    /// table into contiguous chunks. It is worth doing: k-means alone rebuilds
    /// the cube up to four times per export, making this the single largest
    /// serial cost in the pipeline.
    pub fn build_with_threads(palette: &Palette, threads: u32) -> ColorMap {
        Self::build_cancellable(palette, threads, None).expect("uncancelled colour map")
    }

    pub fn build_cancellable(
        palette: &Palette,
        threads: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<ColorMap> {
        check_cancel(cancel)?;
        if palette.colors.len() > 256 {
            return Err(Error::Invalid("palette exceeds 256 colors".into()));
        }
        let mut exact = std::collections::HashMap::new();
        let mut exact_cells = vec![0u64; (Self::SIDE * Self::SIDE * Self::SIDE) / 64];
        let side = Self::SIDE;
        let total = side * side * side;
        let mut lut = vec![0u8; total];
        if palette.colors.is_empty() {
            return Ok(ColorMap {
                lut,
                exact,
                exact_cells,
            });
        }

        let shift = Self::BITS;
        let mask = side - 1;
        let bias = (1i32) << (7 - Self::BITS);

        // Flat index -> cell midpoint. The table is laid out
        // `(rq * side + gq) * side + bq`, so a contiguous chunk of the flat
        // table is a contiguous block of cells and the base offset is all a
        // worker needs to know.
        let fill = |base: usize, out: &mut [u8]| -> Result<()> {
            for (i, slot) in out.iter_mut().enumerate() {
                if i % 256 == 0 {
                    check_cancel(cancel)?;
                }
                let flat = base + i;
                let rq = flat >> (2 * shift);
                let gq = (flat >> shift) & mask;
                let bq = flat & mask;

                let r = ((rq << (8 - Self::BITS)) as i32) + bias;
                let g = ((gq << (8 - Self::BITS)) as i32) + bias;
                let b = ((bq << (8 - Self::BITS)) as i32) + bias;

                let mut best = 0usize;
                let mut best_d = i32::MAX;
                for (i, c) in palette.colors.iter().enumerate() {
                    let dr = r - c[0] as i32;
                    let dg = g - c[1] as i32;
                    let db = b - c[2] as i32;
                    // Weighted to approximate perceptual distance.
                    let d = dr * dr * 3 + dg * dg * 6 + db * db;
                    if d < best_d {
                        best_d = d;
                        best = i;
                    }
                }
                *slot = best.min(255) as u8;
            }
            Ok(())
        };

        let workers = (threads as usize).max(1).min(total);
        if workers <= 1 {
            fill(0, &mut lut)?;
        } else {
            let chunk = total.div_ceil(workers);
            std::thread::scope(|scope| -> Result<()> {
                let mut handles = Vec::new();
                for (i, part) in lut.chunks_mut(chunk).enumerate() {
                    let base = i * chunk;
                    handles.push(scope.spawn(move || fill(base, part)));
                }
                for handle in handles {
                    handle
                        .join()
                        .map_err(|_| Error::Encode("colour map worker panicked".into()))??;
                }
                Ok(())
            })?;
        }

        for (i, c) in palette.colors.iter().enumerate() {
            let key = Self::key(c[0], c[1], c[2]);
            if lut[key] as usize != i {
                exact.insert(*c, i as u8);
                exact_cells[key / 64] |= 1 << (key % 64);
            }
        }
        Ok(ColorMap {
            lut,
            exact,
            exact_cells,
        })
    }

    #[inline]
    pub fn index(&self, r: u8, g: u8, b: u8) -> u8 {
        let key = Self::key(r, g, b);
        if self.exact_cells[key / 64] & (1 << (key % 64)) != 0 {
            self.exact.get(&[r, g, b]).copied().unwrap_or(self.lut[key])
        } else {
            self.lut[key]
        }
    }
}

/// Map a frame to palette indices without dithering.
pub fn map_frame(palette: &Palette, rgb: &[u8], out: &mut Vec<u8>) {
    map_frame_with(&ColorMap::build(palette), rgb, out)
}

/// Same as [`map_frame`], but reuses a colour map the caller already built.
///
/// Building the map costs 64^3 nearest-colour searches, so a caller that maps
/// many frames must build it once and share it instead of paying that per
/// frame. This is where nearly all of the mapping time goes.
pub fn map_frame_with(cm: &ColorMap, rgb: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(rgb.len() / 3);
    for px in rgb.chunks_exact(3) {
        out.push(cm.index(px[0], px[1], px[2]));
    }
}

/// One node of a Wu box: an inclusive range in the 5-bit histogram.
#[derive(Clone, Copy)]
struct Box3 {
    /// Inclusive bounds on every axis, each in `0..=31`.
    lo: [i32; 3],
    hi: [i32; 3],
}

/// Build a palette of at most `max_colors` entries using Wu's algorithm.
pub fn quantize(rgb: &[u8], max_colors: usize) -> Palette {
    quantize_with_threads(rgb, max_colors, 1)
}

/// [`quantize`], with the k-means refinement spread over `threads` workers.
pub fn quantize_with_threads(rgb: &[u8], max_colors: usize, threads: u32) -> Palette {
    quantize_with_options(rgb, max_colors, threads, DEFAULT_KMEANS_ITERATIONS)
}

/// Lloyd iterations used when the caller has no opinion. The export path passes
/// a quality-derived count instead.
pub const DEFAULT_KMEANS_ITERATIONS: usize = 3;

/// Turn the UI's 1..=10 「编码质量」 setting into a k-means iteration count.
///
/// Quality 5 is the default and maps back to three iterations, so leaving the
/// setting alone keeps an export byte-for-byte identical to the old output.
pub fn quantize_iterations(quality: u32) -> usize {
    const TABLE: [usize; 10] = [0, 1, 2, 2, 3, 5, 7, 10, 14, 20];
    TABLE[(quality.clamp(1, 10) - 1) as usize]
}

/// How many levels per channel a `lossy` percentage (0..=100) keeps. Anything
/// below 2 means "leave the pixels alone".
pub fn posterize_levels(lossy: u32) -> u32 {
    let lossy = lossy.min(100);
    if lossy == 0 {
        1
    } else {
        2 + ((100 - lossy) * 254) / 100
    }
}

/// Snap every channel of `rgb` onto a `levels`-step grid, in place.
///
/// This is the whole of the 「有损压缩」 feature: fewer distinct colours make
/// the palette fit better and the LZW stream far more repetitive, at the cost
/// of banding. `levels < 2` is a no-op.
pub fn posterize_in_place(rgb: &mut [u8], levels: u32) {
    if levels < 2 {
        return;
    }
    let steps = levels - 1;
    let mut lut = [0u8; 256];
    for (v, slot) in lut.iter_mut().enumerate() {
        let q = (v as u32 * steps + 127) / 255;
        *slot = ((q * 255 + steps / 2) / steps).min(255) as u8;
    }
    for px in rgb.iter_mut() {
        *px = lut[*px as usize];
    }
}

/// [`quantize_with_threads`], with an explicit k-means iteration count.
pub fn quantize_with_options(
    rgb: &[u8],
    max_colors: usize,
    threads: u32,
    iterations: usize,
) -> Palette {
    quantize_cancellable(rgb, max_colors, threads, iterations, None)
        .expect("uncancelled quantization")
}

pub fn quantize_cancellable(
    rgb: &[u8],
    max_colors: usize,
    threads: u32,
    iterations: usize,
    cancel: Option<&AtomicBool>,
) -> Result<Palette> {
    check_cancel(cancel)?;
    let max_colors = max_colors.clamp(1, 256);
    // Preserve small exact colour sets, including colours in the same 5-bit bin.
    let mut exact = std::collections::BTreeSet::new();
    for (i, px) in rgb.chunks_exact(3).enumerate() {
        if i % 4096 == 0 {
            check_cancel(cancel)?;
        }
        exact.insert([px[0], px[1], px[2]]);
        if exact.len() > max_colors {
            break;
        }
    }
    if exact.len() <= max_colors {
        let mut colors: Vec<_> = exact.into_iter().collect();
        if colors.is_empty() {
            colors.push([0, 0, 0]);
        }
        colors.sort_by_key(|c| c[0] as u32 * 299 + c[1] as u32 * 587 + c[2] as u32 * 114);
        return Ok(Palette { colors });
    }

    // 33^3 tables over the 5-bit-per-channel histogram. Index 0 is a zero
    // border so inclusive prefix sums can be taken with simple recurrences.
    const S: usize = 33;
    let idx = |r: usize, g: usize, b: usize| (r * S + g) * S + b;
    // Per 5-bit bin: count, Σc, Σc² for each of the three channels. The squared
    // sums are what let a box's error be evaluated in closed form.
    const MOM: usize = 7;
    let mut hist = vec![[0f64; MOM]; S * S * S];

    // Accumulate the histogram, remembering the bounding cube of used colours.
    let mut lo = [32i32, 32, 32];
    let mut hi = [0i32, 0, 0];
    let mut any = false;
    for (pi, px) in rgb.chunks_exact(3).enumerate() {
        if pi % 4096 == 0 {
            check_cancel(cancel)?;
        }
        let (r5, g5, b5) = (
            (px[0] >> 3) as usize,
            (px[1] >> 3) as usize,
            (px[2] >> 3) as usize,
        );
        let (r5x, g5x, b5x) = (
            (px[0] >> 3) as i32,
            (px[1] >> 3) as i32,
            (px[2] >> 3) as i32,
        );
        lo = [lo[0].min(r5x), lo[1].min(g5x), lo[2].min(b5x)];
        hi = [hi[0].max(r5x), hi[1].max(g5x), hi[2].max(b5x)];
        any = true;
        let e = &mut hist[idx(r5 + 1, g5 + 1, b5 + 1)]; // 1-based
        e[0] += 1.0;
        e[1] += px[0] as f64;
        e[2] += px[1] as f64;
        e[3] += px[2] as f64;
        e[4] += (px[0] as f64) * (px[0] as f64);
        e[5] += (px[1] as f64) * (px[1] as f64);
        e[6] += (px[2] as f64) * (px[2] as f64);
    }

    if !any {
        return Ok(Palette {
            colors: vec![[0, 0, 0]],
        });
    }

    // Build an inclusive 3-D prefix sum, one axis at a time, in place.
    //
    // In place is not just an optimisation here, it is what makes the sum
    // correct: `cum[p]` has already absorbed its own prefix on this axis, so
    // adding it propagates the whole accumulation. Seeding a second table with
    // the un-updated input and adding `cum[p]` would only ever add a single
    // predecessor — a two-tap sum, not a prefix sum.
    let mut cum = hist;
    for axis in 0..3 {
        for r in 1..S {
            for g in 1..S {
                for b in 1..S {
                    let i = idx(r, g, b);
                    let p = match axis {
                        0 => idx(r - 1, g, b),
                        1 => idx(r, g - 1, b),
                        _ => idx(r, g, b - 1),
                    };
                    for k in 0..MOM {
                        cum[i][k] += cum[p][k];
                    }
                }
            }
        }
    }

    let hist = cum;

    // Moment of a box, read out of the cumulative table in O(1).
    //
    // `hist` is stored 1-based (`hist[coord + 1]`) so the cumulative table needs
    // no special case at zero: an inclusive sum over `[lo, hi]` in 0-based
    // coordinates is simply `cum[hi + 1] - cum[lo]`. Keeping `hi` an *inclusive*
    // index bounded by 31 is what guarantees the `hi + 1` read cannot overrun the
    // 33-entry axis.
    let sum = |m: &[f64], set: &Box3| -> f64 {
        let r0 = set.lo[0].clamp(0, 31) as usize;
        let r1 = (set.hi[0].clamp(0, 31) as usize) + 1;
        let g0 = set.lo[1].clamp(0, 31) as usize;
        let g1 = (set.hi[1].clamp(0, 31) as usize) + 1;
        let b0 = set.lo[2].clamp(0, 31) as usize;
        let b1 = (set.hi[2].clamp(0, 31) as usize) + 1;
        m[idx(r1, g1, b1)] - m[idx(r0, g1, b1)] - m[idx(r1, g0, b1)] + m[idx(r0, g0, b1)]
            - m[idx(r1, g1, b0)]
            + m[idx(r0, g1, b0)]
            + m[idx(r1, g0, b0)]
            - m[idx(r0, g0, b0)]
    };

    // The seven moments are kept as separate flat tables so the readout above is
    // a simple contiguous slice.
    let mut counts = vec![0f64; S * S * S];
    let mut sum_r = vec![0f64; S * S * S];
    let mut sum_g = vec![0f64; S * S * S];
    let mut sum_b = vec![0f64; S * S * S];
    let mut sq_r = vec![0f64; S * S * S];
    let mut sq_g = vec![0f64; S * S * S];
    let mut sq_b = vec![0f64; S * S * S];
    for (i, e) in hist.iter().enumerate() {
        counts[i] = e[0];
        sum_r[i] = e[1];
        sum_g[i] = e[2];
        sum_b[i] = e[3];
        sq_r[i] = e[4];
        sq_g[i] = e[5];
        sq_b[i] = e[6];
    }

    let count_of = |set: &Box3| sum(&counts, set);
    // Sum of squared error of a box: `Σc² - (Σc)²/n` per channel, summed. It is
    // the quantity a split actually reduces, so it reaches zero on a monochrome
    // box and stops the subdivision at the right point.
    let variance = |set: &Box3| -> f64 {
        let c = count_of(set);
        if c <= 0.0 {
            return 0.0;
        }
        let mut v = 0.0;
        for (m, q) in [(&sum_r, &sq_r), (&sum_g, &sq_g), (&sum_b, &sq_b)] {
            let s = sum(m, set);
            v += (sum(q, set) - s * s / c).max(0.0);
        }
        v
    };

    // The starting box spans the colours actually present. Bounds are inclusive
    // 5-bit coordinates, so the whole space is `0..=31` on each axis.
    let mut boxes: Vec<Box3> = vec![Box3 { lo, hi }];

    // Repeatedly split the box whose error is largest. A box with a zero extent
    // on every axis is one colour and cannot be cut further.
    loop {
        check_cancel(cancel)?;
        if boxes.len() >= max_colors {
            break;
        }
        let mut best_i: Option<usize> = None;
        let mut best_score = 0.0;
        for (i, b) in boxes.iter().enumerate() {
            if b.hi == b.lo {
                continue; // a single bin is a single colour
            }
            let score = variance(b);
            if best_i.is_none() || score > best_score {
                best_score = score;
                best_i = Some(i);
            }
        }
        let Some(bi) = best_i.filter(|_| best_score > 0.0) else {
            break;
        };

        let b = boxes[bi];
        // Compare all axes, ignoring cuts that create an empty box.
        let mut best = None;
        let mut best_sum = f64::INFINITY;
        for axis in 0..3 {
            for cut in b.lo[axis]..b.hi[axis] {
                let (mut left, mut right) = (b, b);
                left.hi[axis] = cut;
                right.lo[axis] = cut + 1;
                if count_of(&left) <= 0.0 || count_of(&right) <= 0.0 {
                    continue;
                }
                let score = variance(&left) + variance(&right);
                if score < best_sum {
                    best_sum = score;
                    best = Some((left, right));
                }
            }
        }
        let Some((left, right)) = best else {
            break;
        };
        boxes[bi] = left;
        boxes.push(right);
    }

    // Average colour of each box, weighted by pixel count.
    let mut colors: Vec<[u8; 3]> = Vec::with_capacity(boxes.len());
    let mut weights: Vec<f64> = Vec::with_capacity(boxes.len());
    for b in &boxes {
        let c = count_of(b);
        if c <= 0.0 {
            continue;
        }
        let r = (sum(&sum_r, b) / c).round().clamp(0.0, 255.0) as u8;
        let g = (sum(&sum_g, b) / c).round().clamp(0.0, 255.0) as u8;
        let bl = (sum(&sum_b, b) / c).round().clamp(0.0, 255.0) as u8;
        colors.push([r, g, bl]);
        weights.push(c);
    }

    if colors.is_empty() {
        colors.push([0, 0, 0]);
    }

    improve_with_kmeans(&mut colors, rgb, threads, iterations, cancel)?;
    sort_palette(&mut colors, &mut weights);

    Ok(Palette { colors })
}

/// A few Lloyd iterations pull each palette entry onto the centroid of the
/// colours actually assigned to it, which removes most visible banding.
///
/// This is where the 「编码质量」 setting lands: more iterations cost time in
/// this serial pass and buy accuracy. The loop stops early once nothing moves,
/// so a high count is not automatically a slow one.
fn improve_with_kmeans(
    colors: &mut [[u8; 3]],
    rgb: &[u8],
    threads: u32,
    iterations: usize,
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    if colors.len() < 2 || iterations == 0 {
        return Ok(());
    }

    let iter = iterations.min(64);
    let cube = |c: &[[u8; 3]]| {
        ColorMap::build_cancellable(&Palette { colors: c.to_vec() }, threads, cancel)
    };
    let mut cm = cube(colors)?;

    for iteration in 0..iter {
        check_cancel(cancel)?;
        // Deliberately serial. Splitting this sum across workers would change
        // the order of the floating-point additions, so the palette — and with
        // it every byte of the output — would stop being reproducible for a
        // given input, and threading belongs in the cube build, which is
        // integer-only and therefore identical however it is split.
        let mut acc = vec![[0f64; 4]; colors.len()];
        for (pi, px) in rgb.chunks_exact(3).enumerate() {
            if pi % 4096 == 0 {
                check_cancel(cancel)?;
            }
            let i = cm.index(px[0], px[1], px[2]) as usize;
            if i >= acc.len() {
                continue;
            }
            acc[i][0] += px[0] as f64;
            acc[i][1] += px[1] as f64;
            acc[i][2] += px[2] as f64;
            acc[i][3] += 1.0;
        }
        let mut moved = false;
        for (i, a) in acc.iter().enumerate() {
            if a[3] <= 0.0 {
                continue;
            }
            let nr = (a[0] / a[3]).round().clamp(0.0, 255.0) as u8;
            let ng = (a[1] / a[3]).round().clamp(0.0, 255.0) as u8;
            let nb = (a[2] / a[3]).round().clamp(0.0, 255.0) as u8;
            if [nr, ng, nb] != colors[i] {
                colors[i] = [nr, ng, nb];
                moved = true;
            }
        }
        if !moved {
            break;
        }
        if iteration + 1 < iter {
            cm = cube(colors)?;
        }
    }
    Ok(())
}

/// Order the palette by luminance so consecutive GIF frames share long runs of
/// identical colour tables.
fn sort_palette(colors: &mut [[u8; 3]], weights: &mut [f64]) {
    let mut pairs: Vec<([u8; 3], f64)> = colors
        .iter()
        .copied()
        .zip(weights.iter().copied())
        .collect();
    pairs.sort_by_key(|(c, _)| c[0] as u32 * 299 + c[1] as u32 * 587 + c[2] as u32 * 114);
    for (i, (c, w)) in pairs.into_iter().enumerate() {
        colors[i] = c;
        weights[i] = w;
    }
}

/// Ordered 4x4 Bayer dithering. Strength zero is identical to plain mapping.
pub fn map_frame_ordered_with(
    cm: &ColorMap,
    rgb: &[u8],
    w: usize,
    strength: f64,
    out: &mut Vec<u8>,
) {
    const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    out.clear();
    out.reserve(rgb.len() / 3);
    for (i, px) in rgb.chunks_exact(3).enumerate() {
        let offset = (BAYER[(i / w) % 4][i % w % 4] as f64 - 7.5) * 4.0 * strength.clamp(0.0, 1.0);
        let channel = |v: u8| (v as f64 + offset).round().clamp(0.0, 255.0) as u8;
        out.push(cm.index(channel(px[0]), channel(px[1]), channel(px[2])));
    }
}

/// Quantise with Floyd–Steinberg error diffusion against a fixed palette.
///
/// Slower than the plain mapping path but produces much smoother gradients,
/// which is what makes a photorealistic GIF look good.
pub fn map_frame_dithered(
    palette: &Palette,
    rgb: &[u8],
    w: usize,
    h: usize,
    strength: f64,
    out: &mut Vec<u8>,
) {
    map_frame_dithered_with(&ColorMap::build(palette), palette, rgb, w, h, strength, out)
}

/// Same as [`map_frame_dithered`], but reuses a colour map the caller already
/// built. See [`map_frame_with`] for why that matters.
pub fn map_frame_dithered_with(
    cm: &ColorMap,
    palette: &Palette,
    rgb: &[u8],
    w: usize,
    h: usize,
    strength: f64,
    out: &mut Vec<u8>,
) {
    map_frame_dithered_with_scratch(
        cm,
        palette,
        rgb,
        w,
        h,
        strength,
        out,
        &mut DitherScratch::default(),
    )
}

/// Error rows belong to a worker; every frame starts with zero diffusion error.
#[derive(Default)]
pub(crate) struct DitherScratch {
    rows: [Vec<f32>; 6],
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn map_frame_dithered_with_scratch(
    cm: &ColorMap,
    palette: &Palette,
    rgb: &[u8],
    w: usize,
    h: usize,
    strength: f64,
    out: &mut Vec<u8>,
    scratch: &mut DitherScratch,
) {
    let strength = strength.clamp(0.0, 1.0);
    let n = palette.colors.len();
    if n == 0 {
        out.clear();
        out.resize(w * h, 0);
        return;
    }

    // Two-row rolling error buffers.
    for row in &mut scratch.rows {
        row.resize(w + 2, 0.0);
        row.fill(0.0);
    }
    let [mut err_r, mut err_g, mut err_b, mut nxt_r, mut nxt_g, mut nxt_b] =
        scratch.rows.each_mut();

    out.clear();
    out.reserve(w * h);

    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            if i + 2 >= rgb.len() {
                out.push(0);
                continue;
            }
            let er = err_r[x + 1];
            let eg = err_g[x + 1];
            let eb = err_b[x + 1];
            let r = (rgb[i] as f32 + er * strength as f32).clamp(0.0, 255.0);
            let g = (rgb[i + 1] as f32 + eg * strength as f32).clamp(0.0, 255.0);
            let b = (rgb[i + 2] as f32 + eb * strength as f32).clamp(0.0, 255.0);

            let idx = cm.index(r as u8, g as u8, b as u8);
            out.push(idx);
            let c = palette.colors[idx as usize % n];

            let dr = r - c[0] as f32;
            let dg = g - c[1] as f32;
            let db = b - c[2] as f32;

            // Floyd–Steinberg weights: 7/16 right, 3/16 down-left,
            // 5/16 down, 1/16 down-right.
            err_r[x + 2] += dr * (7.0 / 16.0);
            err_g[x + 2] += dg * (7.0 / 16.0);
            err_b[x + 2] += db * (7.0 / 16.0);

            nxt_r[x] += dr * (3.0 / 16.0);
            nxt_g[x] += dg * (3.0 / 16.0);
            nxt_b[x] += db * (3.0 / 16.0);

            nxt_r[x + 1] += dr * (5.0 / 16.0);
            nxt_g[x + 1] += dg * (5.0 / 16.0);
            nxt_b[x + 1] += db * (5.0 / 16.0);

            nxt_r[x + 2] += dr * (1.0 / 16.0);
            nxt_g[x + 2] += dg * (1.0 / 16.0);
            nxt_b[x + 2] += db * (1.0 / 16.0);
        }
        std::mem::swap(&mut err_r, &mut nxt_r);
        std::mem::swap(&mut err_g, &mut nxt_g);
        std::mem::swap(&mut err_b, &mut nxt_b);
        for v in nxt_r.iter_mut() {
            *v = 0.0;
        }
        for v in nxt_g.iter_mut() {
            *v = 0.0;
        }
        for v in nxt_b.iter_mut() {
            *v = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3-D colour ramp: each channel varies independently so the test data
    /// genuinely occupies the colour cube rather than collapsing onto a line.
    fn gradient(w: usize, h: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                v.push((x * 255 / w.max(1)) as u8);
                v.push((y * 255 / h.max(1)) as u8);
                // Mix both axes so blue is not a function of the other two.
                v.push((((x * 37 + y * 91) % 256) as u8).wrapping_mul(1));
            }
        }
        v
    }

    #[test]
    fn palette_respects_size_bound() {
        let img = gradient(64, 64);
        let p = quantize(&img, 64);
        assert!(p.colors.len() <= 64, "got {}", p.colors.len());
        assert!(!p.colors.is_empty());
        assert!(p.table_bits() <= 8);
        assert_eq!(p.table_bytes().len(), (1usize << p.table_bits()) * 3);
    }

    #[test]
    fn quantisation_error_is_small() {
        let (w, h) = (64usize, 64usize);
        let img = gradient(w, h);
        let p = quantize(&img, 128);
        let cm = ColorMap::build(&p);

        // Ideal error for this palette, by brute-force nearest-neighbour search,
        // so a failure here means the *palette* is bad rather than the mapping.
        let mut ideal = 0f64;
        let mut got = 0f64;
        let mut n = 0f64;
        for px in img.chunks_exact(3) {
            let c = p.colors[cm.index(px[0], px[1], px[2]) as usize];
            let mut best_d = i64::MAX;
            for e in p.colors.iter() {
                let dr = px[0] as i64 - e[0] as i64;
                let dg = px[1] as i64 - e[1] as i64;
                let db = px[2] as i64 - e[2] as i64;
                let d = dr * dr + dg * dg + db * db;
                if d < best_d {
                    best_d = d;
                }
            }
            ideal += best_d as f64;
            for k in 0..3 {
                let d = px[k] as f64 - c[k] as f64;
                got += d * d;
                n += 1.0;
            }
        }
        let rmse = (got / n).sqrt();
        let ideal_rmse = (ideal / n).sqrt();
        // The palette need not be perfect, but it must be close to the best a
        // palette of this size could do on this data — that is what catches a
        // broken split or a broken moment readout.
        assert!(
            rmse <= ideal_rmse * 1.2 + 0.5,
            "mapping is far from optimal: rmse {rmse} vs best possible {ideal_rmse} \
             with {} colours",
            p.colors.len()
        );
    }

    #[test]
    fn color_map_matches_exhaustive_search() {
        // The lookup cube must agree with a linear scan of the palette, give or
        // take the cube's own rounding.
        let img = gradient(32, 32);
        let p = quantize(&img, 32);

        // Compare the cube's choice against a true nearest-neighbour scan.
        let cm = ColorMap::build(&p);
        let mut mismatches = 0usize;
        for px in img.chunks_exact(3) {
            let got = cm.index(px[0], px[1], px[2]) as usize;
            let mut best_d = i32::MAX;
            for c in p.colors.iter() {
                let dr = px[0] as i32 - c[0] as i32;
                let dg = px[1] as i32 - c[1] as i32;
                let db = px[2] as i32 - c[2] as i32;
                let d = dr * dr * 3 + dg * dg * 6 + db * db;
                if d < best_d {
                    best_d = d;
                }
            }
            // Accept any palette entry whose distance is within the cube's own
            // quantisation slack of the true best.
            let g = p.colors[got];
            let dr = px[0] as i32 - g[0] as i32;
            let dg = px[1] as i32 - g[1] as i32;
            let db = px[2] as i32 - g[2] as i32;
            let got_d = dr * dr * 3 + dg * dg * 6 + db * db;
            if got_d > best_d + 4000 {
                mismatches += 1;
            }
        }
        assert_eq!(mismatches, 0, "{mismatches} pixels map poorly");
    }

    #[test]
    fn solid_colour_is_exact() {
        let img: Vec<u8> = std::iter::repeat([12u8, 200, 77])
            .take(100)
            .flatten()
            .collect();
        let p = quantize(&img, 32);
        assert_eq!(p.colors.len(), 1, "one colour should yield one entry");
        assert_eq!(p.colors[0], [12, 200, 77]);
    }

    #[test]
    fn quality_five_preserves_the_historic_output() {
        // The default quality must keep the exact iteration count the exporter
        // used before 「编码质量」 existed, or every existing GIF changes.
        assert_eq!(
            quantize_iterations(export_quality_default()),
            DEFAULT_KMEANS_ITERATIONS
        );
        let mut previous = 0;
        for quality in 1..=10 {
            let iterations = quantize_iterations(quality);
            assert!(
                iterations >= previous,
                "quality {quality} went backwards: {previous} -> {iterations}"
            );
            previous = iterations;
        }
        assert!(quantize_iterations(u32::MAX) <= 64);
    }

    /// The bridge's default, mirrored here so the mapping cannot drift.
    fn export_quality_default() -> u32 {
        5
    }

    #[test]
    fn posterising_quantises_the_grid() {
        assert_eq!(posterize_levels(0), 1, "zero means untouched");
        assert!(posterize_levels(100) < posterize_levels(50));
        let mut px = vec![0u8, 1, 2, 253, 254, 255];
        posterize_in_place(&mut px, 1);
        assert_eq!(px, vec![0, 1, 2, 253, 254, 255], "one level is a no-op");
        assert_eq!(posterize_levels(100), 2);
    }

    #[test]
    fn dithered_mapping_stays_in_range() {
        let (w, h) = (32usize, 32usize);
        let img = gradient(w, h);
        let p = quantize(&img, 32);
        let mut out = Vec::new();
        map_frame_dithered(&p, &img, w, h, 1.0, &mut out);
        assert_eq!(out.len(), w * h);
        let max = p.colors.len() as u8 - 1;
        assert!(out.iter().all(|&i| i <= max));
    }
    #[test]
    fn lossy_grid_gets_coarser_monotonically_and_exact_palettes_remain_exact() {
        let mut previous = 256;
        for lossy in 1..=100 {
            let levels = posterize_levels(lossy);
            assert!(levels <= previous);
            previous = levels;
            let mut values: Vec<u8> = (0..=255).collect();
            posterize_in_place(&mut values, levels);
            values.dedup();
            assert_eq!(values.len(), levels as usize);
        }
        let p = quantize(&[0, 0, 0, 1, 1, 1, 4, 4, 4], 256);
        assert_eq!(p.colors.len(), 3);
        let map = ColorMap::build(&p);
        for c in &p.colors {
            assert_eq!(p.colors[map.index(c[0], c[1], c[2]) as usize], *c);
        }
    }

    #[test]
    fn quantization_and_colour_map_respect_cancellation() {
        let cancel = AtomicBool::new(true);
        assert!(matches!(
            quantize_cancellable(&[0, 0, 0], 2, 1, 20, Some(&cancel)),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            ColorMap::build_cancellable(
                &Palette {
                    colors: vec![[0, 0, 0]]
                },
                1,
                Some(&cancel)
            ),
            Err(Error::Cancelled)
        ));
    }
}
