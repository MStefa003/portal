//! Portal's logo, built into the program: the window icon and the picture
//! next to the name at the top.

use std::io::Cursor;

use iced::widget::image::Handle;
use iced::window;

/// The app icon, as in Explorer and the Start menu.
const PNG: &[u8] = include_bytes!("../../../packaging/icon/app-256.png");

/// The logo's pixels: RGBA, row by row.
struct Pixels {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

fn decode() -> Option<Pixels> {
    let mut decoder = png::Decoder::new(Cursor::new(PNG));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut rgba).ok()?;
    // ALPHA turns every 8-bit image into RGBA (or gray with alpha).
    if info.color_type != png::ColorType::Rgba {
        return None;
    }
    rgba.truncate(info.buffer_size());
    Some(Pixels {
        rgba,
        width: info.width,
        height: info.height,
    })
}

/// The window icon, or none if the logo could not be read.
pub fn icon() -> Option<window::Icon> {
    let pixels = decode()?;
    window::icon::from_rgba(pixels.rgba, pixels.width, pixels.height).ok()
}

/// The logo as a picture to show in the window.
pub fn image() -> Option<Handle> {
    let pixels = decode()?;
    Some(Handle::from_rgba(pixels.width, pixels.height, pixels.rgba))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_built_in_logo_can_be_read() {
        let pixels = super::decode();
        assert!(pixels.is_some_and(|p| p.rgba.len() == (p.width * p.height * 4) as usize));
    }
}
