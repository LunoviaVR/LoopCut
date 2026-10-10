//! Global palette for the GPU-dither engine.
//!
//! [`PaletteBuilder`] feeds sampled composited frames into one imagequant histogram and returns a single palette that
//! every frame of the GIF uses. Frames are added one at a time, so a caller never has to hold all samples in memory.
//! imagequant is deterministic for the same input, so the same export always gets the same palette.

use imagequant::{Attributes, Histogram};

use super::GifError;

/// Most palette entries the GPU-dither engine uses. One more index is reserved for transparency (see
/// [`crate::gif::indexed`]), and GIF allows 256 indices in total.
pub const MAX_PALETTE_COLORS: u16 = 255;

/// Fewest palette entries the GPU-dither engine accepts.
pub const MIN_PALETTE_COLORS: u16 = 2;

/// Collects frames into one imagequant histogram.
pub struct PaletteBuilder {
    attributes: Attributes,
    histogram: Histogram,
    frames: usize,
}

impl PaletteBuilder {
    /// Starts a palette of at most `colors` entries. Values outside `2..=255` are rejected.
    ///
    /// # Errors
    ///
    /// [`GifError::InvalidOptions`] for a colour count outside the range.
    pub fn new(colors: u16) -> Result<Self, GifError> {
        if !(MIN_PALETTE_COLORS..=MAX_PALETTE_COLORS).contains(&colors) {
            return Err(GifError::InvalidOptions(format!(
                "palette colours {colors} must be {MIN_PALETTE_COLORS}..={MAX_PALETTE_COLORS}"
            )));
        }
        let mut attributes = Attributes::new();
        attributes
            .set_max_colors(u32::from(colors))
            .map_err(|err| GifError::Palette(err.to_string()))?;
        let histogram = Histogram::new(&attributes);
        Ok(Self {
            attributes,
            histogram,
            frames: 0,
        })
    }

    /// Adds one frame of straight-alpha RGBA8 pixels (`width * height * 4` bytes). Alpha is ignored: the export
    /// composites onto an opaque background.
    ///
    /// # Errors
    ///
    /// [`GifError::InvalidFrame`] for a buffer of the wrong length, [`GifError::Palette`] when imagequant fails.
    pub fn add_frame(&mut self, rgba: &[u8], width: u32, height: u32) -> Result<(), GifError> {
        let expected = (width as usize) * (height as usize) * 4;
        if width == 0 || height == 0 || rgba.len() != expected {
            return Err(GifError::InvalidFrame(format!(
                "palette frame has {} bytes but {width}x{height} RGBA needs {expected}",
                rgba.len()
            )));
        }
        let pixels: &[imagequant::RGBA] = rgb::bytemuck::cast_slice(rgba);
        let mut image = self
            .attributes
            .new_image_borrowed(pixels, width as usize, height as usize, 0.0)
            .map_err(|err| GifError::Palette(err.to_string()))?;
        self.histogram
            .add_image(&self.attributes, &mut image)
            .map_err(|err| GifError::Palette(err.to_string()))?;
        self.frames += 1;
        Ok(())
    }

    /// Frames added so far.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Quantizes the collected histogram and returns the palette as RGB triples, at most `colors` long.
    ///
    /// # Errors
    ///
    /// [`GifError::NoFrames`] when no frame was added, [`GifError::Palette`] when imagequant fails.
    pub fn finish(mut self) -> Result<Vec<[u8; 3]>, GifError> {
        if self.frames == 0 {
            return Err(GifError::NoFrames);
        }
        let mut result = self
            .histogram
            .quantize(&self.attributes)
            .map_err(|err| GifError::Palette(err.to_string()))?;
        let palette = result.palette_vec();
        Ok(palette.iter().map(|c| [c.r, c.g, c.b]).collect())
    }
}

/// Builds a palette from frames that the caller already holds, for tests and small inputs.
///
/// # Errors
///
/// The errors of [`PaletteBuilder`].
pub fn build_palette<'a>(
    colors: u16,
    frames: impl IntoIterator<Item = &'a [u8]>,
    width: u32,
    height: u32,
) -> Result<Vec<[u8; 3]>, GifError> {
    let mut builder = PaletteBuilder::new(colors)?;
    for frame in frames {
        builder.add_frame(frame, width, height)?;
    }
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gradient with a few hundred distinct colours, so the palette is actually reduced.
    fn gradient(width: u32, height: u32, shift: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                out.push(((x * 7 + shift) % 256) as u8);
                out.push(((y * 5 + shift * 3) % 256) as u8);
                out.push(((x * y + shift) % 256) as u8);
                out.push(255);
            }
        }
        out
    }

    #[test]
    fn palette_size_respects_the_requested_colours() {
        let frame = gradient(64, 48, 0);
        for colors in [2u16, 16, 64, 255] {
            let palette = build_palette(colors, [frame.as_slice()], 64, 48).expect("palette");
            assert!(
                !palette.is_empty(),
                "{colors} colours gave an empty palette"
            );
            assert!(
                palette.len() <= usize::from(colors),
                "{colors} colours gave {} entries",
                palette.len()
            );
        }
    }

    #[test]
    fn palette_is_deterministic_for_the_same_frames() {
        let a = gradient(40, 30, 0);
        let b = gradient(40, 30, 90);
        let first = build_palette(32, [a.as_slice(), b.as_slice()], 40, 30).expect("first");
        let second = build_palette(32, [a.as_slice(), b.as_slice()], 40, 30).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn colour_count_outside_the_range_is_rejected() {
        assert!(matches!(
            PaletteBuilder::new(1),
            Err(GifError::InvalidOptions(_))
        ));
        assert!(matches!(
            PaletteBuilder::new(256),
            Err(GifError::InvalidOptions(_))
        ));
    }

    #[test]
    fn no_frames_is_an_error() {
        let builder = PaletteBuilder::new(16).expect("builder");
        assert!(matches!(builder.finish(), Err(GifError::NoFrames)));
    }

    #[test]
    fn a_wrong_buffer_length_is_rejected() {
        let mut builder = PaletteBuilder::new(16).expect("builder");
        assert!(matches!(
            builder.add_frame(&[0; 7], 2, 1),
            Err(GifError::InvalidFrame(_))
        ));
    }
}
