//! Tray PNG assets only. Status toast is disabled; no toast backgrounds.
use crate::asset_pixels::{byte_len, Image};
use once_cell::sync::OnceCell;
use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GENERIC_READ, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnLoad,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};

static ASSETS: OnceCell<Result<UiAssets, String>> = OnceCell::new();

pub struct UiAssets {
    // Ordered Chinese, English, paused, idle/unknown.
    pub icons: [Image; 4],
}

pub fn get() -> Result<&'static UiAssets, String> {
    ASSETS.get_or_init(|| load_from(&assets_dir()?)).as_ref().map_err(Clone::clone)
}

fn assets_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("executable has no parent")?.join("assets");
    if dir.is_dir() { return Ok(dir); }
    if cfg!(debug_assertions) {
        let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        if dev.is_dir() { return Ok(dev); }
    }
    Err(format!("assets directory missing: {}", dir.display()))
}

pub fn validate() -> Result<(), String> {
    let assets = get()?;
    for icon in &assets.icons {
        for size in [16, 20, 24, 32, 48, 64] {
            let _ = icon.fitted(size, size)?;
        }
    }
    tracing::info!("assets: tray PNG buffers validated; no IME writes");
    Ok(())
}

fn load_from(dir: &Path) -> Result<UiAssets, String> {
    let load = |relative: &str| -> Result<Image, String> {
        let path = dir.join(relative);
        load_png(&path).map_err(|e| format!("{}: {e}", path.display()))
    };
    Ok(UiAssets {
        icons: [
            load("icons/imeflow_zh.png")?,
            load("icons/imeflow_en.png")?,
            load("icons/imeflow_off.png")?,
            load("icons/imeflow.png")?,
        ],
    })
}

struct Apartment {
    owned: bool,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Apartment {
    fn enter() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_err() && hr != RPC_E_CHANGED_MODE { return Err(format!("COM initialization: {hr:?}")); }
        Ok(Self { owned: hr.is_ok(), _thread: std::marker::PhantomData })
    }
}
impl Drop for Apartment {
    fn drop(&mut self) { if self.owned { unsafe { CoUninitialize(); } } }
}

fn load_png(path: &Path) -> Result<Image, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > 32 * 1024 * 1024 { return Err("PNG must be a file smaller than 32 MiB".into()); }
    let mut signature = [0u8; 8];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut signature)).map_err(|e| e.to_string())?;
    if signature != *b"\x89PNG\r\n\x1a\n" { return Err("file is not a PNG (renaming an extension is not conversion)".into()); }
    let _apartment = Apartment::enter()?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let image = unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("WIC factory: {e}"))?;
        let decoder = factory.CreateDecoderFromFilename(PCWSTR(wide.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnLoad)
            .map_err(|e| format!("PNG decode: {e}"))?;
        let frame = decoder.GetFrame(0).map_err(|e| e.to_string())?;
        let (mut width, mut height) = (0u32, 0u32);
        frame.GetSize(&mut width, &mut height).map_err(|e| e.to_string())?;
        let len = byte_len(width as usize, height as usize)?;
        let converter = factory.CreateFormatConverter().map_err(|e| e.to_string())?;
        converter.Initialize(&frame, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .map_err(|e| format!("PNG pixel format: {e}"))?;
        let mut pixels = vec![0u8; len];
        converter.CopyPixels(std::ptr::null(), width * 4, &mut pixels).map_err(|e| e.to_string())?;
        let image = Image::from_straight_bgra(width as usize, height as usize, pixels)?;
        tracing::info!(path = %path.display(), source_w = width, source_h = height,
            crop_w = image.width, crop_h = image.height, "assets: loaded PNG; canvas trimmed in memory");
        image
    };
    let limit = 768.0f64;
    let scale = (limit / image.width.max(image.height) as f64).min(1.0);
    if scale < 1.0 {
        image.resized((image.width as f64 * scale).round().max(1.0) as usize,
            (image.height as f64 * scale).round().max(1.0) as usize)
    } else { Ok(image) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_workspace_assets_decode_without_installing_hooks() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let loaded = load_from(&dir).unwrap();
        assert_eq!(loaded.icons.len(), 4);
        assert!(loaded.icons.iter().all(|i| i.width > 0 && i.height > 0));
        assert_ne!(loaded.icons[0].pixels, loaded.icons[1].pixels);
        assert_ne!(loaded.icons[0].pixels, loaded.icons[2].pixels);
    }
}
