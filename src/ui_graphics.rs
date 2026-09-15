//! Small RAII boundary for GDI objects. No IME or focus manipulation belongs here.
use crate::asset_pixels::{byte_len, Image};
use windows::Win32::Foundation::{BOOL, HANDLE};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO};

pub struct Dib {
    pub dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    len: usize,
}

impl Dib {
    pub fn new(width: usize, height: usize) -> Result<Self, String> {
        let len = byte_len(width, height)?;
        unsafe {
            let dc = CreateCompatibleDC(HDC::default());
            if dc.0.is_null() { return Err("CreateCompatibleDC failed".into()); }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32), // top-down BGRA, matching PNG row order
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
                Ok(bitmap) => bitmap,
                Err(e) => { let _ = DeleteDC(dc); return Err(format!("CreateDIBSection: {e}")); }
            };
            if bits.is_null() {
                let _ = DeleteObject(bitmap); let _ = DeleteDC(dc);
                return Err("CreateDIBSection returned no pixel buffer".into());
            }
            let previous = SelectObject(dc, bitmap);
            if previous.0.is_null() || previous.0 as isize == -1 {
                let _ = DeleteObject(bitmap); let _ = DeleteDC(dc);
                return Err("SelectObject(bitmap) failed".into());
            }
            let mut out = Self { dc, bitmap, previous, bits: bits.cast(), len };
            out.bytes_mut().fill(0);
            Ok(out)
        }
    }

    #[cfg(test)]
    pub fn bytes(&self) -> &[u8] {
        // GDI may batch draws, so synchronize before CPU access to the DIB.
        unsafe { let _ = GdiFlush(); std::slice::from_raw_parts(self.bits, self.len) }
    }
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { let _ = GdiFlush(); std::slice::from_raw_parts_mut(self.bits, self.len) }
    }
    pub fn write(&mut self, image: &Image) -> Result<(), String> {
        if image.pixels.len() != self.len { return Err("DIB pixel length mismatch".into()); }
        self.bytes_mut().copy_from_slice(&image.pixels);
        Ok(())
    }
}
impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            let _ = GdiFlush();
            let _ = SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.dc);
        }
    }
}

pub struct OwnedIcon(pub HICON);
impl OwnedIcon {
    pub fn from_image(image: &Image) -> Result<Self, String> {
        let mut dib = Dib::new(image.width, image.height)?;
        dib.write(image)?;
        // CreateIconIndirect copies both bitmaps. These temporary resources can be freed.
        unsafe {
            let mask_bytes = vec![0u8; ((image.width + 15) / 16) * 2 * image.height];
            let mask = CreateBitmap(image.width as i32, image.height as i32, 1, 1,
                Some(mask_bytes.as_ptr().cast()));
            if mask.0.is_null() { return Err("CreateBitmap(icon mask) failed".into()); }
            // CreateIconIndirect may select the bitmaps internally; do not pass a bitmap
            // still selected into our memory DC.
            let _ = SelectObject(dib.dc, dib.previous);
            let info = ICONINFO { fIcon: BOOL(1), hbmMask: mask, hbmColor: dib.bitmap, ..Default::default() };
            let result = CreateIconIndirect(&info).map(Self).map_err(|e| format!("CreateIconIndirect: {e}"));
            let _ = DeleteObject(mask);
            result
        }
    }
    pub fn into_raw(self) -> HICON {
        let icon = self.0;
        std::mem::forget(self);
        icon
    }
}
impl Drop for OwnedIcon {
    fn drop(&mut self) { unsafe { let _ = DestroyIcon(self.0); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dib_and_icon_can_be_created_and_released_without_a_window() {
        let image = Image::from_straight_bgra(8, 8, [30, 60, 90, 255].repeat(64)).unwrap();
        let _icon = OwnedIcon::from_image(&image).unwrap();
        let mut dib = Dib::new(8, 8).unwrap();
        dib.write(&image).unwrap();
        assert_eq!(dib.bytes(), &image.pixels);
    }
}
