//! The pure parts of drawing the meter into a frame: which swapchains it can be blended into, with which texture
//! format, and where it goes (docs/plans/overlay-parity.md).

use crate::frame::Place;
use ash::vk;

/// The texture format to hold the meter's BGRA bytes for a swapchain of this format and colour space, or None for
/// swapchains the layer leaves alone (HDR colour spaces and float formats, where the bytes would show wrong colours).
/// An sRGB swapchain gets an sRGB texture so the sampled value round-trips to the same bytes.
pub fn texture_format(format: vk::Format, space: vk::ColorSpaceKHR) -> Option<vk::Format> {
    if space != vk::ColorSpaceKHR::SRGB_NONLINEAR {
        return None;
    }
    match format {
        vk::Format::B8G8R8A8_SRGB | vk::Format::R8G8B8A8_SRGB | vk::Format::A8B8G8R8_SRGB_PACK32 => Some(vk::Format::B8G8R8A8_SRGB),
        vk::Format::B8G8R8A8_UNORM
        | vk::Format::R8G8B8A8_UNORM
        | vk::Format::A8B8G8R8_UNORM_PACK32
        | vk::Format::A2B10G10R10_UNORM_PACK32
        | vk::Format::A2R10G10B10_UNORM_PACK32 => Some(vk::Format::B8G8R8A8_UNORM),
        _ => None,
    }
}

/// The meter's top-left corner on an image of `extent`, kept fully inside it. None when the meter does not fit.
pub fn place(w: u32, h: u32, extent: vk::Extent2D, p: Place) -> Option<vk::Offset2D> {
    if w > extent.width || h > extent.height {
        return None;
    }
    let dx = (p.x.clamp(0.0, 1.0) * extent.width as f32).round() as i64;
    let dy = (p.y.clamp(0.0, 1.0) * extent.height as f32).round() as i64;
    let (max_x, max_y) = ((extent.width - w) as i64, (extent.height - h) as i64);
    let x = if p.corner & 1 == 1 { max_x - dx } else { dx };
    let y = if p.corner & 2 == 2 { max_y - dy } else { dy };
    Some(vk::Offset2D { x: x.clamp(0, max_x) as i32, y: y.clamp(0, max_y) as i32 })
}

/// Where a tooltip of w × h goes for a pointer at (x, y): below and right of it, or on the other side where it would
/// leave the image, and always inside it.
pub fn beside(x: i32, y: i32, w: u32, h: u32, extent: vk::Extent2D) -> vk::Offset2D {
    const GAP: i32 = 18;
    let (w, h, iw, ih) = (w as i32, h as i32, extent.width as i32, extent.height as i32);
    let tx = if x + GAP + w <= iw { x + GAP } else { x - GAP - w };
    let ty = if y + GAP + h <= ih { y + GAP } else { y - GAP - h };
    vk::Offset2D { x: tx.clamp(0, (iw - w).max(0)), y: ty.clamp(0, (ih - h).max(0)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_placement() {
        let srgb = vk::ColorSpaceKHR::SRGB_NONLINEAR;
        assert_eq!(texture_format(vk::Format::R8G8B8A8_UNORM, srgb), Some(vk::Format::B8G8R8A8_UNORM), "the game's swapchain");
        assert_eq!(texture_format(vk::Format::B8G8R8A8_SRGB, srgb), Some(vk::Format::B8G8R8A8_SRGB));
        assert_eq!(texture_format(vk::Format::A2B10G10R10_UNORM_PACK32, srgb), Some(vk::Format::B8G8R8A8_UNORM), "10-bit SDR is fine");
        assert_eq!(texture_format(vk::Format::A2B10G10R10_UNORM_PACK32, vk::ColorSpaceKHR::HDR10_ST2084_EXT), None, "HDR is left alone");
        assert_eq!(texture_format(vk::Format::R16G16B16A16_SFLOAT, srgb), None);
        let e = vk::Extent2D { width: 2560, height: 1440 };
        let at = |p| place(340, 440, e, p).map(|o| (o.x, o.y));
        assert_eq!(at(Place { corner: 0, x: 0.01, y: 0.25 }), Some((26, 360)));
        assert_eq!(at(Place { corner: 1, x: 0.0, y: 0.0 }), Some((2220, 0)), "top-right");
        assert_eq!(at(Place { corner: 3, x: 0.01, y: 0.01 }), Some((2194, 986)), "bottom-right");
        assert_eq!(at(Place { corner: 0, x: 0.99, y: 0.99 }), Some((2220, 1000)), "kept inside the image");
        assert_eq!(place(340, 440, vk::Extent2D { width: 300, height: 1000 }, Place::default()), None, "does not fit");
        let at = |x, y| { let o = beside(x, y, 300, 200, e); (o.x, o.y) };
        assert_eq!(at(100, 100), (118, 118), "below and right of the pointer");
        assert_eq!(at(2500, 1400), (2182, 1182), "above and left near the far corner");
        assert_eq!(at(100, 1300), (118, 1082), "above near the bottom");
    }
}
