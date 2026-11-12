//! Accessible color palettes derived from artwork pixels.
//!
//! [`from_pixels`] turns sampled, opaque artwork pixels into a [`Palette`]
//! whose colors meet WCAG 2.x contrast targets against the configured
//! background. Decoding and downsampling happen before this crate (in
//! `yt-core`), so this crate is pure math: no file, network, image-format or
//! UI work, and equal input always produces an equal palette.

mod color;
mod generated;
mod palette;
mod quantize;
#[cfg(test)]
mod tests;

pub use color::Rgb;
pub use generated::generated_palette;
pub use palette::{
    from_pixels, Palette, PaletteOptions, AMBIENT_COUNT, AMBIENT_TEXT, DEFAULT_BACKGROUND,
    DEFAULT_MIN_TEXT_CONTRAST, ON_ACCENT_DARK, ON_ACCENT_LIGHT,
};

/// Why a palette could not be produced.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ThemeError {
    /// No pixels were given: the artwork had no opaque pixels to sample.
    #[error("artwork has no opaque pixels")]
    NoOpaquePixels,
    /// A [`PaletteOptions`] field is outside its documented range.
    #[error("invalid palette options: {0}")]
    InvalidOptions(&'static str),
}
