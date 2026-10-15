//! A from-scratch GIF89a writer.
//!
//! Only what a converter needs: a global colour table, per-frame local colour
//! tables, an optional loop extension, and LZW-compressed image data written in
//! 255-byte sub-blocks.

use std::io::Write;

use super::quantize::Palette;
use crate::error::{Error, Result};

/// Disposal method for the previous frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposal {
    /// Leave the frame in place (0).
    Keep = 0,
    /// Explicit "do not dispose" (1). Required before a frame that relies on
    /// the previous one showing through its transparent pixels.
    DoNotDispose = 1,
    /// Clear to background before the next frame (2).
    Background = 2,
}

/// Settings for one frame.
pub struct FrameSpec<'a> {
    pub width: u16,
    pub height: u16,
    /// Palette indices, exactly `width * height` entries.
    pub indices: &'a [u8],
    /// Palette the indices refer to.
    pub palette: &'a Palette,
    /// Use a local colour table for this frame.
    pub local_table: bool,
    /// Delay in hundredths of a second.
    pub delay_cs: u16,
    pub disposal: Disposal,
    /// Index that means "keep what is already on the canvas". `None` writes a
    /// plain fully-opaque frame.
    pub transparent: Option<u8>,
}

/// Validate one frame before anything is written for it.
fn validate_frame(f: &FrameSpec<'_>) -> Result<()> {
    if f.width == 0 || f.height == 0 {
        return Err(Error::Encode("GIF frame has a zero dimension".into()));
    }
    if f.indices.len() != f.width as usize * f.height as usize {
        return Err(Error::Encode(format!(
            "frame has {} indices, expected {} for {}x{}",
            f.indices.len(),
            f.width as usize * f.height as usize,
            f.width,
            f.height
        )));
    }
    if f.palette.colors.is_empty() || f.palette.colors.len() > 256 {
        return Err(Error::Encode(format!(
            "palette has {} entries, must be 1..=256",
            f.palette.colors.len()
        )));
    }
    Ok(())
}

/// Incremental GIF89a writer.
///
/// Frames are appended one at a time, so a caller can index a bounded batch of
/// frames and hand them straight to the sink rather than holding every frame's
/// palette indices in memory at once. `write_gif` is the convenience wrapper
/// for callers that already have the whole sequence in hand.
pub struct GifWriter<W: Write> {
    out: W,
}

impl<W: Write> GifWriter<W> {
    pub fn new(
        mut out: W,
        canvas_w: u16,
        canvas_h: u16,
        global: Option<&Palette>,
        loop_forever: bool,
    ) -> Result<Self> {
        if canvas_w == 0 || canvas_h == 0 {
            return Err(Error::Encode("GIF canvas has a zero dimension".into()));
        }

        // Header.
        out.write_all(b"GIF89a")?;

        // Logical screen descriptor.
        out.write_all(&canvas_w.to_le_bytes())?;
        out.write_all(&canvas_h.to_le_bytes())?;
        match global {
            Some(p) if !p.colors.is_empty() => {
                let bits = p.table_bits();
                let size_field = bits - 1;
                out.write_all(&[0b1111_0000 | size_field])?; // global table, 8-bit colour
                out.write_all(&[0])?; // background colour index
                out.write_all(&[0])?; // pixel aspect ratio
                out.write_all(&p.table_bytes())?;
            }
            _ => {
                out.write_all(&[0b0111_0000])?; // no global table, 8-bit colour
                out.write_all(&[0])?;
                out.write_all(&[0])?;
            }
        }

        // Netscape looping extension.
        if loop_forever {
            out.write_all(&[0x21, 0xFF, 0x0B])?;
            out.write_all(b"NETSCAPE2.0")?;
            out.write_all(&[0x03, 0x01, 0x00, 0x00, 0x00])?;
        }

        Ok(Self { out })
    }

    /// Append one frame.
    pub fn add_frame(&mut self, f: &FrameSpec<'_>) -> Result<()> {
        let block = frame_block(f)?;
        self.out.write_all(&block)?;
        Ok(())
    }

    /// Append a block produced earlier by [`frame_block`].
    ///
    /// Frames are independent, so a caller that wants to compress on several
    /// threads can build blocks out of order and then feed them here in the
    /// right order. No validation happens: the block already passed it.
    pub fn add_block(&mut self, block: &[u8]) -> Result<()> {
        self.out.write_all(block)?;
        Ok(())
    }

    /// Write the trailer and hand the sink back.
    pub fn finish(mut self) -> Result<W> {
        self.out.write_all(&[0x3B])?; // trailer
        Ok(self.out)
    }
}

/// Serialise one frame — control extension, image descriptor and LZW data —
/// into a standalone byte block.
///
/// Splitting this out of [`GifWriter::add_frame`] is what lets the export
/// compress frames on worker threads: the expensive part of a frame is the
/// palette mapping plus the LZW pass, and neither touches the output stream.
pub fn frame_block(f: &FrameSpec<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    frame_block_into(
        f,
        FrameRegion::full(f.width, f.height),
        &mut LzwScratch::default(),
        &mut out,
    )?;
    Ok(out)
}

/// A nonempty rectangle within a full, mapped canvas.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FrameRegion {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
}

impl FrameRegion {
    pub fn full(width: u16, height: u16) -> Self {
        Self {
            left: 0,
            top: 0,
            width,
            height,
        }
    }
}

/// Reuse the block and dictionary allocations; read rectangle rows without a packed copy.
pub(crate) fn frame_block_into(
    f: &FrameSpec<'_>,
    region: FrameRegion,
    scratch: &mut LzwScratch,
    out: &mut Vec<u8>,
) -> Result<()> {
    validate_frame(f)?;
    if region.width == 0
        || region.height == 0
        || region.left as u32 + region.width as u32 > f.width as u32
        || region.top as u32 + region.height as u32 > f.height as u32
    {
        return Err(Error::Encode(
            "GIF frame rectangle lies outside the canvas".into(),
        ));
    }
    out.clear();

    // Graphic control extension.
    let mut packed = ((f.disposal as u8) & 0x07) << 2;
    let transparent = f.transparent.unwrap_or(0);
    if f.transparent.is_some() {
        packed |= 0x01; // "a transparent colour index follows"
    }
    out.extend_from_slice(&[0x21, 0xF9, 0x04, packed]);
    out.extend_from_slice(&f.delay_cs.to_le_bytes());
    out.extend_from_slice(&[transparent]);
    out.extend_from_slice(&[0x00]); // block terminator

    // Image descriptor.
    out.extend_from_slice(&[0x2C]);
    out.extend_from_slice(&region.left.to_le_bytes());
    out.extend_from_slice(&region.top.to_le_bytes());
    out.extend_from_slice(&region.width.to_le_bytes());
    out.extend_from_slice(&region.height.to_le_bytes());

    let table_bits = f.palette.table_bits();
    if f.local_table {
        out.push(0b1000_0000 | (table_bits - 1));
        out.extend_from_slice(&f.palette.table_bytes());
    } else {
        out.push(0b0000_0000);
    }

    // Image data. GIF clamps the LZW minimum code size to at least 2, even
    // when the colour table itself only needs one bit (a 2-entry table).
    let lzw_bits = table_bits.max(2);
    out.push(lzw_bits);
    let rows = f
        .indices
        .chunks_exact(f.width as usize)
        .skip(region.top as usize)
        .take(region.height as usize);
    let indices = rows.flat_map(|row| {
        row[region.left as usize..region.left as usize + region.width as usize]
            .iter()
            .copied()
    });
    let compressed = scratch.compress(
        indices,
        region.width as usize * region.height as usize,
        lzw_bits,
    )?;
    write_sub_blocks(out, compressed)?;
    out.push(0x00); // block terminator

    Ok(())
}

/// Rewrite the delay field of an already-built frame block.
///
/// The 「复用重复帧」 optimisation writes one frame and then stretches its
/// delay while the following frames are identical to it. Returns `false` (and
/// changes nothing) if `block` is not a block this function understands.
pub fn set_block_delay(block: &mut [u8], delay_cs: u16) -> bool {
    // 0x21 0xF9 0x04 <packed> <delay lo> <delay hi> <transparent> 0x00
    if block.len() < 8 || block[0] != 0x21 || block[1] != 0xF9 || block[2] != 0x04 {
        return false;
    }
    let [lo, hi] = delay_cs.to_le_bytes();
    block[4] = lo;
    block[5] = hi;
    true
}

/// Read back the delay of a block built by [`frame_block`].
pub fn block_delay(block: &[u8]) -> Option<u16> {
    if block.len() < 8 || block[0] != 0x21 || block[1] != 0xF9 || block[2] != 0x04 {
        return None;
    }
    Some(u16::from_le_bytes([block[4], block[5]]))
}

pub fn write_gif<W: Write>(
    out: &mut W,
    canvas_w: u16,
    canvas_h: u16,
    global: Option<&Palette>,
    loop_forever: bool,
    frames: &[FrameSpec<'_>],
) -> Result<()> {
    if frames.is_empty() {
        return Err(Error::Encode("no frames to encode".into()));
    }
    // Validate every frame up front: this form writes into the caller's sink,
    // and failing half way through would leave a truncated GIF behind.
    for f in frames {
        validate_frame(f)?;
    }

    let mut writer = GifWriter::new(out, canvas_w, canvas_h, global, loop_forever)?;
    for f in frames {
        writer.add_frame(f)?;
    }
    writer.finish()?;
    Ok(())
}

/// Write data in the GIF sub-block format: length byte, then that many bytes.
fn write_sub_blocks<W: Write>(out: &mut W, data: &[u8]) -> Result<()> {
    for chunk in data.chunks(255) {
        out.write_all(&[chunk.len() as u8])?;
        out.write_all(chunk)?;
    }
    Ok(())
}

/// A bit sink that accumulates codes LSB-first, as GIF requires.
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    nbits: u32,
}

impl BitWriter {
    fn new(capacity_hint: usize) -> BitWriter {
        BitWriter {
            out: Vec::with_capacity(capacity_hint),
            acc: 0,
            nbits: 0,
        }
    }

    #[inline]
    fn push(&mut self, code: u16, width: u8) {
        self.acc |= (code as u32) << self.nbits;
        self.nbits += width as u32;
        while self.nbits >= 8 {
            self.out.push((self.acc & 0xFF) as u8);
            self.acc >>= 8;
            self.nbits -= 8;
        }
    }

    fn flush(&mut self) {
        if self.nbits > 0 {
            self.out.push((self.acc & 0xFF) as u8);
        }
        self.acc = 0;
        self.nbits = 0;
    }

    #[cfg(test)]
    fn finish(mut self) -> Vec<u8> {
        self.flush();
        self.out
    }
}

/// GIF-flavoured LZW compression.
///
/// `min_code_size` is the palette's table bit count; indices must all be below
/// `1 << min_code_size`.
pub fn lzw_compress(indices: &[u8], min_code_size: u8) -> Result<Vec<u8>> {
    let mut scratch = LzwScratch::default();
    scratch.compress(indices.iter().copied(), indices.len(), min_code_size)?;
    Ok(scratch.bits.out)
}

/// One dictionary and bit buffer per worker, reset before every frame.
pub(crate) struct LzwScratch {
    keys: Vec<u32>,
    vals: Vec<u16>,
    bits: BitWriter,
}

impl Default for LzwScratch {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            vals: Vec::new(),
            bits: BitWriter::new(0),
        }
    }
}

impl LzwScratch {
    fn compress(
        &mut self,
        indices: impl Iterator<Item = u8>,
        pixels: usize,
        min_code_size: u8,
    ) -> Result<&[u8]> {
        if !(2..=8).contains(&min_code_size) {
            return Err(Error::Encode(format!(
                "invalid LZW minimum code size {min_code_size}"
            )));
        }

        let clear: u16 = 1u16 << min_code_size;
        let eoi: u16 = clear + 1;
        let max_code: u16 = 4095;

        // Dictionary: (prefix code, byte) → code. A flat table keyed by
        // prefix*256 + byte is exact and fast up to 4096 entries, so use a small
        // open-addressing map instead of a hash map to stay allocation-light.
        const TABLE_SIZE: usize = 8192;
        self.keys.resize(TABLE_SIZE, u32::MAX);
        self.keys.fill(u32::MAX);
        self.vals.resize(TABLE_SIZE, 0);
        let keys = &mut self.keys;
        let vals = &mut self.vals;
        let mask = TABLE_SIZE - 1;

        let w = &mut self.bits;
        w.out.clear();
        w.out.reserve(pixels / 2 + 16);
        w.acc = 0;
        w.nbits = 0;
        let mut width = min_code_size + 1;
        let mut next_code = eoi + 1;

        w.push(clear, width);

        let mut cur: u16 = 0;
        let mut have_cur = false;

        for px in indices {
            if (px as u16) >= clear {
                return Err(Error::Encode(format!(
                    "colour index {px} does not fit in {} bits",
                    min_code_size
                )));
            }
            let c = px as u16;
            if !have_cur {
                cur = c;
                have_cur = true;
                continue;
            }

            let key = ((cur as u32) << 8) | c as u32;
            let mut slot = (key.wrapping_mul(2654435761) as usize) & mask;
            let found = loop {
                match keys[slot] {
                    u32::MAX => break None,
                    k if k == key => break Some(vals[slot]),
                    _ => slot = (slot + 1) & mask,
                }
            };

            if let Some(code) = found {
                cur = code;
                continue;
            }

            // Emit the longest match.
            w.push(cur, width);

            if next_code <= max_code {
                keys[slot] = key;
                vals[slot] = next_code;
                next_code += 1;
                if next_code > (1u16 << width) && width < 12 {
                    width += 1;
                }
            } else {
                // The dictionary is full. Tell the reader to start over, then begin
                // the next sequence from `c` WITHOUT recording `(cur, c)`: a reader
                // that has just seen CLEAR treats the following code as the first of
                // a new sequence and adds no entry for it. Recording one here would
                // leave the two dictionaries a step apart and corrupt the stream.
                w.push(clear, width);
                keys.fill(u32::MAX);
                next_code = eoi + 1;
                width = min_code_size + 1;
            }
            cur = c;
        }

        if have_cur {
            w.push(cur, width);
        }
        w.push(eoi, width);
        w.flush();
        Ok(&w.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gif::quantize::Palette;

    #[test]
    fn bit_writer_packs_lsb_first() {
        let mut w = BitWriter::new(8);
        w.push(0b001, 3);
        w.push(0b11001, 5);
        let out = w.finish();
        // First code occupies the low 3 bits, second the next 5.
        assert_eq!(out, vec![0b11001_001]);
    }

    #[test]
    fn lzw_rejects_out_of_range_indices() {
        let e = lzw_compress(&[0, 4], 2).unwrap_err();
        assert!(matches!(e, Error::Encode(_)));
    }

    #[test]
    fn writes_a_minimal_stream() {
        let pal = Palette {
            colors: vec![[0, 0, 0], [255, 255, 255]],
        };
        let indices = vec![0u8, 1, 0, 1, 1, 1, 0, 0, 1];
        let frames = [FrameSpec {
            width: 3,
            height: 3,
            indices: &indices,
            palette: &pal,
            local_table: false,
            delay_cs: 10,
            disposal: Disposal::Keep,
            transparent: None,
        }];
        let mut buf = Vec::new();
        write_gif(&mut buf, 3, 3, Some(&pal), true, &frames).unwrap();
        assert_eq!(&buf[0..6], b"GIF89a");
        assert_eq!(buf[6], 3);
        assert_eq!(buf[7], 0);
        assert_eq!(buf[8], 3);
        assert_eq!(buf[9], 0);
        assert_eq!(*buf.last().unwrap(), 0x3B);
        // Contains the netscape loop block.
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains("NETSCAPE2.0"));
    }

    /// A GIF LZW reader written straight from the specification, so the encoder
    /// can be checked without depending on an external tool that might quietly
    /// recover from a malformed stream.
    struct LzwReader<'a> {
        data: &'a [u8],
        pos: usize,
        acc: u32,
        nbits: u32,
    }

    impl<'a> LzwReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            LzwReader {
                data,
                pos: 0,
                acc: 0,
                nbits: 0,
            }
        }

        fn read(&mut self, width: u8) -> Option<u16> {
            while self.nbits < width as u32 {
                let byte = *self.data.get(self.pos)?;
                self.pos += 1;
                self.acc |= (byte as u32) << self.nbits;
                self.nbits += 8;
            }
            let code = (self.acc & ((1u32 << width) - 1)) as u16;
            self.acc >>= width;
            self.nbits -= width as u32;
            Some(code)
        }
    }

    /// Decode a raw LZW block (the payload following the minimum code size
    /// byte). Returns the pixels and how many CLEAR codes were seen mid-stream,
    /// so tests can prove they actually exercised a dictionary reset.
    fn lzw_decompress(data: &[u8], min_code_size: u8) -> Result<(Vec<u8>, usize)> {
        let clear: u16 = 1u16 << min_code_size;
        let eoi = clear + 1;
        let max_entries = 1usize << 12;

        let mut dict: Vec<Vec<u8>> = Vec::with_capacity(max_entries);
        let reset = |dict: &mut Vec<Vec<u8>>| {
            dict.clear();
            for i in 0..clear {
                dict.push(vec![i as u8]);
            }
            dict.push(Vec::new()); // clear
            dict.push(Vec::new()); // eoi
        };
        reset(&mut dict);

        let mut width = min_code_size + 1;
        let mut reader = LzwReader::new(data);
        let mut out: Vec<u8> = Vec::new();
        let mut prev: Option<u16> = None;
        let mut resets = 0usize;

        loop {
            let code = match reader.read(width) {
                Some(c) => c,
                None => break,
            };
            if code == clear {
                // The leading CLEAR is part of the format; only count the ones
                // that appear after data has been emitted.
                if !out.is_empty() {
                    resets += 1;
                }
                reset(&mut dict);
                width = min_code_size + 1;
                prev = None;
                continue;
            }
            if code == eoi {
                break;
            }

            let entry: Vec<u8> = if (code as usize) < dict.len() {
                dict[code as usize].clone()
            } else if let Some(p) = prev {
                // The KwKwK case: the code being read is the one this very step
                // is about to define.
                let base = &dict[p as usize];
                if base.is_empty() {
                    return Err(Error::Encode(format!("bad LZW code {code}")));
                }
                let mut e = base.clone();
                e.push(base[0]);
                e
            } else {
                return Err(Error::Encode(format!("LZW code {code} out of range")));
            };

            out.extend_from_slice(&entry);

            if let Some(p) = prev {
                if dict.len() < max_entries {
                    let base = &dict[p as usize];
                    if base.is_empty() {
                        return Err(Error::Encode("bad LZW prefix".into()));
                    }
                    let mut ne = base.clone();
                    ne.push(entry[0]);
                    dict.push(ne);
                    if width < 12 && dict.len() == (1usize << width) {
                        width += 1;
                    }
                }
            }
            prev = Some(code);
        }

        Ok((out, resets))
    }

    #[test]
    fn lzw_round_trips_small_input() {
        let indices: Vec<u8> = vec![0, 1, 2, 3, 0, 1, 2, 3, 3, 3, 2, 1, 0];
        let packed = lzw_compress(&indices, 2).unwrap();
        let (back, _) = lzw_decompress(&packed, 2).unwrap();
        assert_eq!(back, indices);
    }

    #[test]
    fn lzw_round_trips_every_code_width() {
        // Enough distinct 2-byte sequences that the dictionary grows well past
        // every power-of-two boundary from 6 up to 4096, forcing width changes.
        for min_code_size in [2u8, 4, 8] {
            let span = 1u32 << min_code_size;
            let mut indices: Vec<u8> = Vec::new();
            let mut state: u32 = 12345;
            for _ in 0..60_000 {
                state = state.wrapping_mul(1103515245).wrapping_add(12345);
                indices.push(((state >> 16) % span) as u8);
            }
            let packed = lzw_compress(&indices, min_code_size).unwrap();
            let (back, _) = lzw_decompress(&packed, min_code_size).unwrap();
            assert_eq!(
                back, indices,
                "round trip failed at min_code_size {min_code_size}"
            );
        }
    }

    #[test]
    fn lzw_round_trips_across_a_dictionary_reset() {
        // 4096 entries exist at 8-bit roots, so a long stream of fresh pairs
        // exhausts the dictionary and makes the encoder emit CLEAR mid-stream.
        // Deliberately walk through many unseen bigrams rather than repeating a
        // small cycle, otherwise the dictionary never fills.
        let mut indices: Vec<u8> = Vec::new();
        let mut a: u16 = 7;
        let mut b: u16 = 199;
        for _ in 0..120_000 {
            a = (a * 5 + 17) % 256;
            b = (b * 7 + 91 + a) % 256;
            indices.push(a as u8);
            indices.push(b as u8);
        }
        let packed = lzw_compress(&indices, 8).unwrap();
        let (back, resets) = lzw_decompress(&packed, 8).unwrap();
        assert!(resets > 0, "this input never filled the dictionary");
        assert_eq!(back.len(), indices.len(), "decoded length differs");
        assert!(back == indices, "decoded content differs");
    }

    #[test]
    fn lzw_handles_a_single_pixel() {
        let packed = lzw_compress(&[9], 4).unwrap();
        let (back, resets) = lzw_decompress(&packed, 4).unwrap();
        assert_eq!(back, vec![9]);
        assert_eq!(resets, 0);
    }

    #[test]
    fn lzw_handles_a_constant_run() {
        // A constant buffer exercises the longest-match growth path.
        let indices = vec![3u8; 20_000];
        let packed = lzw_compress(&indices, 2).unwrap();
        let (back, _) = lzw_decompress(&packed, 2).unwrap();
        assert_eq!(back, indices);
        // A run of one value must compress dramatically.
        assert!(packed.len() < indices.len() / 20);
    }

    #[test]
    fn reused_dictionary_resets_between_frames_and_after_errors() {
        let mut scratch = LzwScratch::default();
        for bits in [8, 2, 4, 8, 2] {
            let input: Vec<_> = (0..100_000u32)
                .map(|i| ((i * 97 + i / 19) % (1 << bits)) as u8)
                .collect();
            let packed = scratch
                .compress(input.iter().copied(), input.len(), bits)
                .unwrap();
            assert_eq!(lzw_decompress(packed, bits).unwrap().0, input);
            assert!(scratch.compress([0, 4].into_iter(), 2, 2).is_err());
        }
    }

    #[test]
    fn rectangle_reads_only_selected_rows_and_rejects_out_of_bounds() {
        let palette = Palette {
            colors: vec![[0, 0, 0], [255, 255, 255]],
        };
        let indices = [0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0];
        let frame = FrameSpec {
            width: 4,
            height: 3,
            indices: &indices,
            palette: &palette,
            local_table: false,
            delay_cs: 10,
            disposal: Disposal::DoNotDispose,
            transparent: None,
        };
        let region = FrameRegion {
            left: 1,
            top: 1,
            width: 2,
            height: 2,
        };
        let mut block = Vec::new();
        frame_block_into(&frame, region, &mut LzwScratch::default(), &mut block).unwrap();
        assert_eq!(&block[9..17], &[1, 0, 1, 0, 2, 0, 2, 0]);
        let mut data = Vec::new();
        let mut p = 19;
        while block[p] != 0 {
            let n = block[p] as usize;
            data.extend_from_slice(&block[p + 1..p + 1 + n]);
            p += 1 + n;
        }
        assert_eq!(
            lzw_decompress(&data, block[18]).unwrap().0,
            vec![0, 1, 1, 1]
        );
        assert!(frame_block_into(
            &frame,
            FrameRegion { left: 3, ..region },
            &mut LzwScratch::default(),
            &mut block
        )
        .is_err());
    }
}
