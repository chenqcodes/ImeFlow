//! Pure pixel operations. Source PNGs are never modified.
//! All pixels after `from_straight_bgra` are premultiplied BGRA, including resampling.

#[derive(Clone, Debug)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

pub fn byte_len(width: usize, height: usize) -> Result<usize, String> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err("image dimensions must be within 1..=4096".into());
    }
    width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "image buffer overflow".into())
}

impl Image {
    pub fn from_straight_bgra(
        width: usize,
        height: usize,
        mut pixels: Vec<u8>,
    ) -> Result<Self, String> {
        if pixels.len() != byte_len(width, height)? {
            return Err("image buffer length mismatch".into());
        }
        // Generated assets sometimes contain a solid white/black canvas rather than alpha.
        // Only remove edge-connected canvas pixels. Enclosed glyphs and dark interiors survive.
        let opaque = pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
        if opaque {
            let corners = [0, width - 1, (height - 1) * width, height * width - 1];
            let black_canvas = corners
                .iter()
                .all(|&i| pixels[i * 4..i * 4 + 3].iter().all(|&v| v <= 8));
            let is_canvas = |i: usize, data: &[u8]| {
                let p = &data[i * 4..i * 4 + 4];
                p[3] != 0
                    && if black_canvas {
                        p[..3].iter().all(|&v| v <= 8)
                    } else {
                        p[..3].iter().all(|&v| v >= 232)
                    }
            };
            let mut stack = Vec::new();
            for x in 0..width {
                stack.push(x);
                stack.push((height - 1) * width + x);
            }
            for y in 0..height {
                stack.push(y * width);
                stack.push(y * width + width - 1);
            }
            // Mark on insertion, not on removal, so the work queue stays bounded.
            let mut queue = Vec::new();
            for i in stack {
                if is_canvas(i, &pixels) {
                    pixels[i * 4 + 3] = 0;
                    queue.push(i);
                }
            }
            while let Some(i) = queue.pop() {
                let x = i % width;
                let y = i / width;
                let neighbours = [
                    (x > 0).then(|| i - 1),
                    (x + 1 < width).then(|| i + 1),
                    (y > 0).then(|| i - width),
                    (y + 1 < height).then(|| i + width),
                ];
                for j in neighbours.into_iter().flatten() {
                    if is_canvas(j, &pixels) {
                        pixels[j * 4 + 3] = 0;
                        queue.push(j);
                    }
                }
            }
        }
        for p in pixels.as_chunks_mut::<4>().0.iter_mut() {
            let a = u16::from(p[3]);
            for c in &mut p[..3] {
                *c = ((u16::from(*c) * a + 127) / 255) as u8;
            }
        }
        let image = Self {
            width,
            height,
            pixels,
        };
        image.trim()
    }

    fn trim(self) -> Result<Self, String> {
        // Ignore almost-transparent isolated glow pixels when finding useful content.
        let mut bounds = (self.width, self.height, 0usize, 0usize);
        let mut found = false;
        for y in 0..self.height {
            for x in 0..self.width {
                if self.pixels[(y * self.width + x) * 4 + 3] > 16 {
                    bounds.0 = bounds.0.min(x);
                    bounds.1 = bounds.1.min(y);
                    bounds.2 = bounds.2.max(x);
                    bounds.3 = bounds.3.max(y);
                    found = true;
                }
            }
        }
        if !found {
            return Err("image contains no visible content".into());
        }
        let (left, top, right, bottom) = bounds;
        let width = right - left + 1;
        let height = bottom - top + 1;
        let mut pixels = Vec::with_capacity(byte_len(width, height)?);
        for y in top..=bottom {
            let start = (y * self.width + left) * 4;
            pixels.extend_from_slice(&self.pixels[start..start + width * 4]);
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Area resampling instead of nearest-neighbour/bilinear-only downsampling.
    /// It retains thin strokes when a large logo is reduced to a 16px tray cell.
    pub fn resized(&self, width: usize, height: usize) -> Result<Self, String> {
        let mut pixels = vec![0u8; byte_len(width, height)?];
        let sx = self.width as f64 / width as f64;
        let sy = self.height as f64 / height as f64;
        for y in 0..height {
            let y0 = y as f64 * sy;
            let y1 = (y + 1) as f64 * sy;
            for x in 0..width {
                let x0 = x as f64 * sx;
                let x1 = (x + 1) as f64 * sx;
                let mut sums = [0.0f64; 4];
                for iy in y0.floor() as usize..(y1.ceil() as usize).min(self.height) {
                    let wy = (y1.min((iy + 1) as f64) - y0.max(iy as f64)).max(0.0);
                    for ix in x0.floor() as usize..(x1.ceil() as usize).min(self.width) {
                        let wx = (x1.min((ix + 1) as f64) - x0.max(ix as f64)).max(0.0);
                        let i = (iy * self.width + ix) * 4;
                        for (c, sum) in sums.iter_mut().enumerate() {
                            *sum += f64::from(self.pixels[i + c]) * wx * wy;
                        }
                    }
                }
                let i = (y * width + x) * 4;
                for c in 0..4 {
                    pixels[i + c] = (sums[c] / (sx * sy)).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Preserve the source proportions; never stretch a wide pill into a square.
    pub fn fitted(&self, width: usize, height: usize) -> Result<Self, String> {
        let mut pixels = vec![0u8; byte_len(width, height)?];
        let scale = (width as f64 / self.width as f64).min(height as f64 / self.height as f64);
        let w = ((self.width as f64 * scale).round() as usize).clamp(1, width);
        let h = ((self.height as f64 * scale).round() as usize).clamp(1, height);
        let image = self.resized(w, h)?;
        let left = (width - w) / 2;
        let top = (height - h) / 2;
        for y in 0..h {
            let offset = ((top + y) * width + left) * 4;
            pixels[offset..offset + w * 4]
                .copy_from_slice(&image.pixels[y * w * 4..(y + 1) * w * 4]);
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    #[cfg(test)]
    /// Fill the box (may crop). `align` 0=left/top bias, 0.5=center, 1=right/bottom.
    pub fn fitted_cover(&self, width: usize, height: usize, align: f64) -> Result<Self, String> {
        let align = align.clamp(0.0, 1.0);
        let scale = (width as f64 / self.width as f64).max(height as f64 / self.height as f64);
        let w = ((self.width as f64 * scale).round() as usize).max(width);
        let h = ((self.height as f64 * scale).round() as usize).max(height);
        let image = self.resized(w, h)?;
        let left = ((w - width) as f64 * align).round() as usize;
        let top = ((h - height) as f64 * 0.5).round() as usize;
        let mut pixels = vec![0u8; byte_len(width, height)?];
        for y in 0..height {
            let src = ((top + y) * w + left) * 4;
            let dst = y * width * 4;
            pixels[dst..dst + width * 4].copy_from_slice(&image.pixels[src..src + width * 4]);
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    #[cfg(test)]
    /// The text mask is black with white grayscale-antialiased glyphs (not ClearType).
    pub fn overlay_mask(&mut self, mask: &[u8], color_bgr: [u8; 3]) -> Result<(), String> {
        if mask.len() != self.pixels.len() {
            return Err("text mask dimensions mismatch".into());
        }
        for (dst, src) in self
            .pixels
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(mask.as_chunks::<4>().0.iter())
        {
            let a = u32::from(src[0].max(src[1]).max(src[2]));
            for c in 0..3 {
                dst[c] = ((u32::from(color_bgr[c]) * a + u32::from(dst[c]) * (255 - a) + 127) / 255)
                    as u8;
            }
            dst[3] = (a + (u32::from(dst[3]) * (255 - a) + 127) / 255).min(255) as u8;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_empty_oversized_and_wrong_buffers() {
        assert!(byte_len(0, 40).is_err());
        assert!(byte_len(usize::MAX, 40).is_err());
        assert!(Image::from_straight_bgra(2, 2, vec![0; 4]).is_err());
        assert!(Image::from_straight_bgra(2, 2, vec![0; 16]).is_err());
    }
    #[test]
    fn removes_canvas_not_enclosed_white_glyph() {
        let mut p = vec![255; 7 * 7 * 4];
        for y in 1..6 {
            for x in 1..6 {
                p[(y * 7 + x) * 4..(y * 7 + x) * 4 + 3].fill(30);
            }
        }
        p[(3 * 7 + 3) * 4..(3 * 7 + 3) * 4 + 4].fill(255);
        let image = Image::from_straight_bgra(7, 7, p).unwrap();
        assert_eq!((image.width, image.height), (5, 5));
        assert_eq!(
            &image.pixels[(2 * 5 + 2) * 4..(2 * 5 + 2) * 4 + 4],
            &[255; 4]
        );
    }
    #[test]
    fn removes_black_canvas_without_erasing_enclosed_dark_interior() {
        let mut p = [0, 0, 0, 255].repeat(25);
        for y in 1..4 {
            for x in 1..4 {
                p[(y * 5 + x) * 4..(y * 5 + x) * 4 + 4].copy_from_slice(&[40, 70, 90, 255]);
            }
        }
        p[(2 * 5 + 2) * 4..(2 * 5 + 2) * 4 + 3].fill(0);
        let image = Image::from_straight_bgra(5, 5, p).unwrap();
        assert_eq!((image.width, image.height), (3, 3));
        assert_eq!(&image.pixels[16..16 + 4], &[0, 0, 0, 255]);
    }
    #[test]
    fn resizing_keeps_alpha_premultiplied() {
        let image =
            Image::from_straight_bgra(2, 1, vec![100, 150, 200, 128, 0, 0, 0, 255]).unwrap();
        let small = image.resized(1, 1).unwrap();
        assert!(small.pixels[..3].iter().all(|&v| v <= small.pixels[3]));
    }
    #[test]
    fn fitted_pill_is_centered_not_distorted() {
        let image = Image::from_straight_bgra(4, 2, [30, 60, 90, 255].repeat(8)).unwrap();
        let out = image.fitted(4, 4).unwrap();
        assert_eq!(out.pixels[3], 0);
        assert_eq!(out.pixels[4 * 4 + 3], 255);
        assert_eq!(out.pixels[3 * 4 * 4 + 3], 0);
    }
    #[test]
    fn fitted_cover_fills_square_and_respects_align() {
        // 8x2 bar: left half blue-ish, right half green-ish (premultiply later via from_straight).
        let mut p = Vec::new();
        for _ in 0..2 {
            for x in 0..8 {
                if x < 4 {
                    p.extend_from_slice(&[200, 80, 40, 255]);
                } else {
                    p.extend_from_slice(&[40, 200, 80, 255]);
                }
            }
        }
        let image = Image::from_straight_bgra(8, 2, p).unwrap();
        let left = image.fitted_cover(2, 2, 0.0).unwrap();
        let right = image.fitted_cover(2, 2, 1.0).unwrap();
        assert_eq!((left.width, left.height), (2, 2));
        // Left crop should be closer to the warm channel; right crop to the green channel.
        assert!(left.pixels[0] > left.pixels[1]);
        assert!(right.pixels[1] > right.pixels[0]);
    }
    #[test]
    fn text_composite_updates_color_and_alpha() {
        let mut image = Image {
            width: 1,
            height: 1,
            pixels: vec![0; 4],
        };
        image.overlay_mask(&[128, 128, 128, 0], [255; 3]).unwrap();
        assert_eq!(image.pixels, vec![128; 4]);
        assert!(image.overlay_mask(&[], [255; 3]).is_err());
    }
}
