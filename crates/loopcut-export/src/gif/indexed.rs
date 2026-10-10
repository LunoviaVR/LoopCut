//! GIF writer for indexed frames that share one global palette.
//!
//! Each frame is either the whole canvas or, when smaller, only the rectangle that changed since the previous frame.
//! Unchanged pixels inside that rectangle use a transparent index, which keeps the previous frame on screen (dispose
//! method "keep"). The transparent index is `palette.len()`, so the palette is padded to a power of two that holds it.
//!
//! Delays are the plan's centiseconds, written as given, so every delay is exact. Each frame is one GIF frame record:
//! identical frames are not merged, which keeps the frame count and the timing equal to the plan.

use std::io::Write;

use gif::{Encoder, Frame, Repeat};

use super::GifError;
use super::palette::MAX_PALETTE_COLORS;

/// Writes indexed frames one at a time. Call [`IndexedWriter::finish`] to write the trailer.
pub struct IndexedWriter<W: Write> {
    encoder: Encoder<W>,
    width: u16,
    height: u16,
    /// The transparent index, which is `palette.len()`.
    transparent: u8,
    /// The global colour table, padded to a power of two. Kept to measure candidate frames.
    table: Vec<u8>,
    /// Indices of the frame on screen after the last push.
    previous: Option<Vec<u8>>,
    /// Buffer the size measurements encode into. It is emptied after each measurement and reused, so a frame costs no
    /// allocation for its measurement once the first one has grown it.
    scratch: Vec<u8>,
    frames: usize,
}

impl<W: Write> IndexedWriter<W> {
    /// Starts a GIF of `width` by `height` pixels with `palette` as its global colour table.
    ///
    /// `loop_count` follows the project model: 0 loops forever, 1 plays once, N plays N times.
    ///
    /// # Errors
    ///
    /// [`GifError::InvalidOptions`] for a size outside `1..=65535`, or an empty palette or one longer than
    /// [`MAX_PALETTE_COLORS`]. [`GifError::Io`] when the header cannot be written.
    pub fn new(
        out: W,
        width: u32,
        height: u32,
        palette: &[[u8; 3]],
        loop_count: u16,
    ) -> Result<Self, GifError> {
        let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
            return Err(GifError::InvalidOptions(format!(
                "GIF size {width}x{height} exceeds 65535 pixels a side"
            )));
        };
        if width == 0 || height == 0 {
            return Err(GifError::InvalidOptions(format!(
                "GIF size {width}x{height} must not be empty"
            )));
        }
        if palette.is_empty() || palette.len() > usize::from(MAX_PALETTE_COLORS) {
            return Err(GifError::InvalidOptions(format!(
                "palette of {} entries must be 1..={MAX_PALETTE_COLORS}",
                palette.len()
            )));
        }
        let transparent = palette.len() as u8;
        let entries = (palette.len() + 1).next_power_of_two().max(2);
        let mut table = Vec::with_capacity(entries * 3);
        for color in palette {
            table.extend_from_slice(color);
        }
        table.resize(entries * 3, 0);
        let mut encoder = Encoder::new(out, width, height, &table).map_err(encoding_error)?;
        encoder
            .set_repeat(repeat_for_loop_count(loop_count))
            .map_err(encoding_error)?;
        Ok(Self {
            encoder,
            width,
            height,
            transparent,
            table,
            previous: None,
            scratch: Vec::new(),
            frames: 0,
        })
    }

    /// Frames written so far.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Appends one frame of palette indices (`width * height` bytes, row-major) shown for `delay_cs` centiseconds.
    ///
    /// # Errors
    ///
    /// [`GifError::InvalidFrame`] for a buffer of the wrong length or an index outside the palette,
    /// [`GifError::Io`] when the frame cannot be written.
    pub fn push(&mut self, indices: &[u8], delay_cs: u16) -> Result<(), GifError> {
        let expected = usize::from(self.width) * usize::from(self.height);
        if indices.len() != expected {
            return Err(GifError::InvalidFrame(format!(
                "frame has {} indices but the GIF is {}x{}",
                indices.len(),
                self.width,
                self.height
            )));
        }
        if let Some(&bad) = indices.iter().find(|&&index| index >= self.transparent) {
            return Err(GifError::InvalidFrame(format!(
                "index {bad} is outside the {}-entry palette",
                self.transparent
            )));
        }
        let frame = match self.previous.take() {
            None => self.frame(
                0,
                0,
                self.width,
                self.height,
                indices.to_vec(),
                None,
                delay_cs,
            ),
            Some(previous) => {
                let frame = self.changed_frame(&previous, indices, delay_cs);
                self.previous = Some(previous);
                frame?
            }
        };
        self.encoder.write_frame(&frame).map_err(encoding_error)?;
        self.frames += 1;
        match self.previous.as_mut() {
            Some(previous) => previous.copy_from_slice(indices),
            None => self.previous = Some(indices.to_vec()),
        }
        Ok(())
    }

    /// Writes the trailer and returns the output.
    ///
    /// # Errors
    ///
    /// [`GifError::Io`] when the trailer cannot be written.
    pub fn finish(self) -> Result<W, GifError> {
        self.encoder.into_inner().map_err(encoding_error)
    }

    /// The frame that changes `previous` into `current`: the changed rectangle with transparency when that is smaller
    /// than the whole canvas, otherwise the whole canvas.
    fn changed_frame(
        &mut self,
        previous: &[u8],
        current: &[u8],
        delay_cs: u16,
    ) -> Result<Frame<'static>, GifError> {
        let width = usize::from(self.width);
        let height = usize::from(self.height);
        let mut bounds: Option<(usize, usize, usize, usize)> = None;
        for y in 0..height {
            for x in 0..width {
                let i = y * width + x;
                if previous[i] != current[i] {
                    bounds = Some(match bounds {
                        None => (x, y, x + 1, y + 1),
                        Some((x0, y0, x1, y1)) => {
                            (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1))
                        }
                    });
                }
            }
        }
        let Some((x0, y0, x1, y1)) = bounds else {
            // Nothing changed. One transparent pixel keeps the canvas as it is.
            return Ok(self.frame(
                0,
                0,
                1,
                1,
                vec![self.transparent],
                Some(self.transparent),
                delay_cs,
            ));
        };
        let (rect_width, rect_height) = (x1 - x0, y1 - y0);
        let mut patch = Vec::with_capacity(rect_width * rect_height);
        for y in y0..y1 {
            for x in x0..x1 {
                let i = y * width + x;
                patch.push(if previous[i] == current[i] {
                    self.transparent
                } else {
                    current[i]
                });
            }
        }
        let masked = self.frame(
            x0 as u16,
            y0 as u16,
            rect_width as u16,
            rect_height as u16,
            patch,
            Some(self.transparent),
            delay_cs,
        );
        // Both forms are measured even when the rectangle covers the canvas: transparent pixels inside the canvas can
        // cost more than they save, so the smaller form is written.
        let whole = self.frame(
            0,
            0,
            self.width,
            self.height,
            current.to_vec(),
            None,
            delay_cs,
        );
        if self.encoded_len(&masked)? <= self.encoded_len(&whole)? {
            Ok(masked)
        } else {
            Ok(whole)
        }
    }

    /// Bytes one frame costs in this GIF, measured by encoding it alone with the same colour table. The measurement
    /// encodes into `self.scratch`, which keeps its allocation for the next one.
    fn encoded_len(&mut self, frame: &Frame<'_>) -> Result<usize, GifError> {
        let buffer = std::mem::take(&mut self.scratch);
        let mut encoder =
            Encoder::new(buffer, self.width, self.height, &self.table).map_err(encoding_error)?;
        encoder.write_frame(frame).map_err(encoding_error)?;
        let mut buffer = encoder.into_inner().map_err(encoding_error)?;
        let len = buffer.len();
        buffer.clear();
        self.scratch = buffer;
        Ok(len)
    }

    #[allow(clippy::too_many_arguments)]
    fn frame(
        &self,
        left: u16,
        top: u16,
        width: u16,
        height: u16,
        pixels: Vec<u8>,
        transparent: Option<u8>,
        delay_cs: u16,
    ) -> Frame<'static> {
        let mut frame = Frame::from_indexed_pixels(width, height, pixels, transparent);
        frame.left = left;
        frame.top = top;
        frame.delay = delay_cs;
        frame
    }
}

/// Maps the project's loop count onto the GIF repeat count, as the Quality engine does.
pub fn repeat_for_loop_count(loop_count: u16) -> Repeat {
    match loop_count {
        0 => Repeat::Infinite,
        1 => Repeat::Finite(0),
        plays => Repeat::Finite(plays - 1),
    }
}

fn encoding_error(err: gif::EncodingError) -> GifError {
    match err {
        gif::EncodingError::Io(io_err) => GifError::Io(io_err),
        other => GifError::Internal(format!("GIF encoder: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Black, white, red and blue.
    const PALETTE: [[u8; 3]; 4] = [[0, 0, 0], [255, 255, 255], [255, 0, 0], [0, 0, 255]];

    /// Decodes every frame as palette indices, with the delays, and composes them onto a canvas as a viewer does.
    /// Returns the canvas after each frame, the delays, and the repeat setting.
    fn decode_canvases(bytes: &[u8], width: u16, height: u16) -> (Vec<Vec<u8>>, Vec<u16>, Repeat) {
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::Indexed);
        let mut decoder = options.read_info(Cursor::new(bytes)).expect("header");
        let repeat = decoder.repeat();
        let mut canvas = vec![0u8; usize::from(width) * usize::from(height)];
        let mut canvases = Vec::new();
        let mut delays = Vec::new();
        while let Some(frame) = decoder.read_next_frame().expect("frame") {
            delays.push(frame.delay);
            let transparent = frame.transparent;
            let (left, top) = (usize::from(frame.left), usize::from(frame.top));
            let fw = usize::from(frame.width);
            for row in 0..usize::from(frame.height) {
                for col in 0..fw {
                    let value = frame.buffer[row * fw + col];
                    if Some(value) == transparent {
                        continue;
                    }
                    let index = (top + row) * usize::from(width) + left + col;
                    canvas[index] = value;
                }
            }
            canvases.push(canvas.clone());
        }
        (canvases, delays, repeat)
    }

    #[test]
    fn frames_keep_their_delays_and_the_canvas_matches_the_indices() {
        let (w, h) = (6u32, 4u32);
        let frames: Vec<Vec<u8>> = vec![
            (0..24).map(|i| (i % 4) as u8).collect(),
            (0..24).map(|i| ((i + 1) % 4) as u8).collect(),
            (0..24).map(|i| if i < 12 { 2 } else { 3 }).collect(),
            (0..24).map(|i| if i < 12 { 2 } else { 3 }).collect(),
        ];
        let delays = [5u16, 7, 2, 9];
        let mut out = Vec::new();
        let mut writer = IndexedWriter::new(&mut out, w, h, &PALETTE, 0).expect("writer");
        for (frame, &delay) in frames.iter().zip(&delays) {
            writer.push(frame, delay).expect("push");
        }
        assert_eq!(writer.frames(), 4);
        writer.finish().expect("finish");

        let (canvases, decoded_delays, repeat) = decode_canvases(&out, w as u16, h as u16);
        assert_eq!(decoded_delays, delays);
        assert_eq!(repeat, Repeat::Infinite);
        assert_eq!(canvases.len(), 4);
        for (index, (canvas, expected)) in canvases.iter().zip(&frames).enumerate() {
            assert_eq!(canvas, expected, "canvas after frame {index}");
        }
    }

    /// Length of a GIF written from `frames` (all shown for 5 cs) with a fresh writer.
    fn gif_len(w: u32, h: u32, frames: &[&[u8]]) -> usize {
        let mut out = Vec::new();
        let mut writer = IndexedWriter::new(&mut out, w, h, &PALETTE, 0).expect("writer");
        for frame in frames {
            writer.push(frame, 5).expect("push");
        }
        writer.finish().expect("finish");
        out.len()
    }

    #[test]
    fn unchanged_frames_cost_less_than_the_first_frame() {
        let (w, h) = (64u32, 48u32);
        let first: Vec<u8> = (0..(w * h)).map(|i| ((i * 13) % 4) as u8).collect();
        let header_and_trailer = gif_len(w, h, &[]);
        let one = gif_len(w, h, &[first.as_slice()]);
        let two = gif_len(w, h, &[first.as_slice(), first.as_slice()]);
        let first_cost = one - header_and_trailer;
        let identical_cost = two - one;
        assert!(
            identical_cost * 4 < first_cost,
            "an identical frame costs {identical_cost} bytes, the first {first_cost}"
        );
    }

    #[test]
    fn a_small_change_is_written_as_a_rectangle() {
        let (w, h) = (32u32, 32u32);
        let base: Vec<u8> = vec![1; (w * h) as usize];
        let mut changed = base.clone();
        for y in 10..13 {
            for x in 20..23 {
                changed[(y * w + x) as usize] = 2;
            }
        }
        let mut out = Vec::new();
        let mut writer = IndexedWriter::new(&mut out, w, h, &PALETTE, 0).expect("writer");
        writer.push(&base, 5).expect("base");
        writer.push(&changed, 5).expect("changed");
        writer.finish().expect("finish");
        let (canvases, _, _) = decode_canvases(&out, w as u16, h as u16);
        assert_eq!(canvases[1], changed, "the rectangle restores the change");
    }

    #[test]
    fn a_change_that_covers_the_canvas_round_trips() {
        let (w, h) = (16u32, 8u32);
        let base: Vec<u8> = (0..w * h).map(|i| (i % 4) as u8).collect();
        // Every pixel changes: the rectangle is the canvas, and the whole-canvas form competes with it.
        let all: Vec<u8> = base.iter().map(|&v| (v + 1) % 4).collect();
        // Only the two corners change: the rectangle is still the canvas, and the unchanged pixels between them stay
        // transparent.
        let corners: Vec<u8> = base
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                if i == 0 || i == (w * h - 1) as usize {
                    3 - v
                } else {
                    v
                }
            })
            .collect();
        let mut out = Vec::new();
        let mut writer = IndexedWriter::new(&mut out, w, h, &PALETTE, 0).expect("writer");
        writer.push(&base, 5).expect("base");
        writer.push(&all, 5).expect("all");
        writer.push(&corners, 5).expect("corners");
        writer.finish().expect("finish");
        let (canvases, _, _) = decode_canvases(&out, w as u16, h as u16);
        assert_eq!(canvases[0], base);
        assert_eq!(
            canvases[1], all,
            "a full-canvas change restores every pixel"
        );
        assert_eq!(canvases[2], corners, "a corner change keeps the rest");
    }

    #[test]
    fn a_covering_change_is_never_bigger_than_the_whole_canvas() {
        let (w, h) = (64u32, 48u32);
        let base: Vec<u8> = (0..w * h).map(|i| ((i * 7) % 4) as u8).collect();
        // Every pixel changes except a scattering of unchanged ones, so the changed rectangle covers the canvas and its
        // unchanged pixels are transparent.
        let changed: Vec<u8> = base
            .iter()
            .enumerate()
            .map(|(i, &v)| if i % 5 == 0 { v } else { (v + 1) % 4 })
            .collect();
        // The same two frames written whole, with the same colour table and delays.
        let mut table: Vec<u8> = PALETTE.iter().flatten().copied().collect();
        table.resize(8 * 3, 0);
        let mut whole_only = Vec::new();
        let mut encoder =
            Encoder::new(&mut whole_only, w as u16, h as u16, &table).expect("encoder");
        encoder.set_repeat(Repeat::Infinite).expect("repeat");
        for pixels in [&base, &changed] {
            let mut frame = Frame::from_indexed_pixels(w as u16, h as u16, pixels.clone(), None);
            frame.delay = 5;
            encoder.write_frame(&frame).expect("whole frame");
        }
        drop(encoder);

        let mut out = Vec::new();
        let mut writer = IndexedWriter::new(&mut out, w, h, &PALETTE, 0).expect("writer");
        writer.push(&base, 5).expect("base");
        writer.push(&changed, 5).expect("changed");
        writer.finish().expect("finish");
        assert!(
            out.len() <= whole_only.len(),
            "the writer wrote {} bytes, the whole canvas {}",
            out.len(),
            whole_only.len()
        );
        let (canvases, _, _) = decode_canvases(&out, w as u16, h as u16);
        assert_eq!(canvases[1], changed, "the change is restored");
    }

    #[test]
    fn loop_count_follows_the_project_model() {
        assert_eq!(repeat_for_loop_count(0), Repeat::Infinite);
        assert_eq!(repeat_for_loop_count(1), Repeat::Finite(0));
        assert_eq!(repeat_for_loop_count(4), Repeat::Finite(3));
        for (loop_count, expected) in [
            (0u16, Repeat::Infinite),
            (1, Repeat::Finite(0)),
            (3, Repeat::Finite(2)),
        ] {
            let mut out = Vec::new();
            // A decoder needs at least one image before it reports the repeat setting.
            let mut writer =
                IndexedWriter::new(&mut out, 4, 4, &PALETTE, loop_count).expect("writer");
            writer.push(&[0; 16], 5).expect("frame");
            writer.finish().expect("finish");
            let (_, _, repeat) = decode_canvases(&out, 4, 4);
            assert_eq!(repeat, expected, "loop count {loop_count}");
        }
    }

    #[test]
    fn bad_input_is_rejected_without_writing_a_frame() {
        let mut out = Vec::new();
        let mut writer = IndexedWriter::new(&mut out, 4, 4, &PALETTE, 0).expect("writer");
        assert!(matches!(
            writer.push(&[0; 15], 5),
            Err(GifError::InvalidFrame(_))
        ));
        // Index 4 is the transparent slot, which a frame may not use for colour.
        assert!(matches!(
            writer.push(&[4; 16], 5),
            Err(GifError::InvalidFrame(_))
        ));
        assert_eq!(writer.frames(), 0);
    }

    #[test]
    fn palette_and_size_limits_are_enforced() {
        let mut out = Vec::new();
        assert!(matches!(
            IndexedWriter::new(&mut out, 4, 4, &[], 0),
            Err(GifError::InvalidOptions(_))
        ));
        assert!(matches!(
            IndexedWriter::new(&mut out, 0, 4, &PALETTE, 0),
            Err(GifError::InvalidOptions(_))
        ));
        let too_many = vec![[0u8; 3]; 256];
        assert!(matches!(
            IndexedWriter::new(&mut out, 4, 4, &too_many, 0),
            Err(GifError::InvalidOptions(_))
        ));
    }
}
