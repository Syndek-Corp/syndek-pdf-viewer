//! DocuView's Windows print backend.
//!
//! The print-ready PDF (one page per sheet, composed by `pdfcraft-print`) is rendered sheet by
//! sheet with the app's own renderer at the printer's resolution and handed to the driver through
//! the classic GDI print path (`StartDoc` / `StartPage` / `StretchDIBits` / `EndPage` / `EndDoc`),
//! which is how Acrobat, SumatraPDF and Chrome drive a v3 printer driver.
//!
//! Why not the XPS path (`Windows.Data.Pdf` + Direct2D print control) that upstream uses: on a
//! v3 driver Windows rasterises every page at the driver's resolution anyway, then converts the
//! XPS to GDI page by page. Measured on a 10-page office document with the Toshiba Universal PS3
//! driver at 600 dpi: 44 s on the client and 19 MB of PostScript per page. This path, same driver,
//! same document: 3–7 s, and each sheet is sent in the smallest form the driver takes:
//!
//! - black-and-white sheets (text, line art) as 1-bit at the printer's resolution, which is what
//!   the engine prints for text anyway: 0.3–1 MB a sheet;
//! - grey sheets as 8-bit grey;
//! - colour sheets as JPEG when the driver is a PostScript one (it passes JPEG through as
//!   `DCTDecode`, ~3 MB a sheet) and as 24-bit otherwise (PCL drivers compress on their own).
//!
//! Copies, collation, two-sided, colour, paper and orientation go through `DEVMODE`, validated by
//! the driver (`DocumentProperties`). Sheets of the other orientation turn the job with `ResetDC`
//! between pages. All GDI calls are in this crate, wrapped so callers see only `Result`.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::sync::Arc;

use pdfcraft_render::{PRINT_TAG, PageRenderer, RenderRequest, Tile, device_pixels, effective_scale};

/// Two-sided printing. `Default` leaves the printer's own setting in force.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Duplex {
    #[default]
    Default,
    Off,
    LongEdge,
    ShortEdge,
}

#[derive(Clone, Debug)]
pub struct Job<'a> {
    /// `None`: the user's default printer.
    pub printer: Option<&'a str>,
    pub copies: u32,
    pub collate: bool,
    pub duplex: Duplex,
    pub grayscale: bool,
    pub title: &'a str,
    /// Sheets larger than the paper are shrunk to its printable area and centred; smaller ones
    /// keep their size, centred. Otherwise every sheet is placed at its physical size at the
    /// paper's corner, and the paper must be the sheet's size.
    pub shrink_to_fit: bool,
}

/// A sheet's size in points (after rotation), as the print-ready PDF displays it.
fn sheet_sizes(pdf: &Arc<Vec<u8>>) -> Result<Vec<(f32, f32)>, String> {
    let info = pdfcraft_render::inspect(Arc::clone(pdf), None).map_err(|e| format!("the print-ready PDF can't be read: {e}"))?;
    let sizes: Vec<(f32, f32)> = info.pages.iter().map(|p| (p.width, p.height)).collect();
    if sizes.is_empty() {
        return Err("the print-ready PDF has no pages".into());
    }
    Ok(sizes)
}

/// Render sheet `page` at `scale` device pixels per point as premultiplied RGBA: `(width,
/// height, pixels)`. A sheet the renderer can draw in one go (its caps: 8192 px a side, 64 M
/// pixels) is rendered whole; a larger one (Legal and Tabloid at 600 dpi, anything bigger) is
/// rendered in tiles at the full scale and stitched, so no sheet is sent below the printer's
/// resolution. A sheet that would need more than [`MAX_SHEET_BYTES`] falls back to the
/// renderer's capped whole-page render, which the driver then scales up.
fn render_sheet(renderer: &mut PageRenderer, page: usize, w_pt: f32, h_pt: f32, scale: f32) -> Result<(u32, u32, Vec<u8>), String> {
    let n = page + 1;
    let capped = effective_scale(w_pt, h_pt, scale);
    let (full_w, full_h) = (device_pixels(w_pt, scale), device_pixels(h_pt, scale));
    let bytes = u64::from(full_w) * u64::from(full_h) * 4;
    if capped >= scale * 0.999 || bytes > MAX_SHEET_BYTES || full_w == 0 || full_h == 0 {
        let r = renderer.render(RenderRequest { page, scale: capped, tag: PRINT_TAG, ..Default::default() });
        if let Some(e) = r.error {
            return Err(format!("sheet {n} could not be rendered: {e}"));
        }
        if r.width == 0 || r.height == 0 {
            return Err(format!("sheet {n} rendered empty"));
        }
        return Ok((r.width, r.height, r.rgba.to_vec()));
    }
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(bytes as usize).map_err(|_| format!("sheet {n}: not enough memory for a {full_w} x {full_h} render"))?;
    rgba.resize(bytes as usize, 0);
    let fw = full_w as usize;
    for ty in (0..full_h).step_by(TILE as usize) {
        for tx in (0..full_w).step_by(TILE as usize) {
            let (tw, th) = (TILE.min(full_w - tx), TILE.min(full_h - ty));
            let r =
                renderer.render(RenderRequest { page, scale, tile: Some(Tile { x: tx, y: ty, w: tw, h: th }), tag: PRINT_TAG, ..Default::default() });
            if let Some(e) = r.error {
                return Err(format!("sheet {n} could not be rendered (tile at {tx}, {ty}): {e}"));
            }
            let (rw, rh) = (r.width as usize, r.height as usize);
            let (cw, ch) = (rw.min(tw as usize), rh.min(th as usize));
            if cw < tw as usize || ch < th as usize {
                return Err(format!("sheet {n}: tile at {tx}, {ty} came back {rw} x {rh}, not {tw} x {th}"));
            }
            let src = r.rgba.to_vec();
            for row in 0..ch {
                let (Some(from), Some(to)) = (
                    src.get(row * rw * 4..(row * rw + cw) * 4),
                    rgba.get_mut(((ty as usize + row) * fw + tx as usize) * 4..((ty as usize + row) * fw + tx as usize + cw) * 4),
                ) else {
                    return Err(format!("sheet {n}: tile at {tx}, {ty} is out of bounds"));
                };
                to.copy_from_slice(from);
            }
        }
    }
    Ok((full_w, full_h, rgba))
}

/// The renderer draws a tile of at most this many pixels a side.
const TILE: u32 = 4096;

/// Beyond this (an A0 sheet at 600 dpi is 1.4 GB) the sheet is rendered at the renderer's
/// capped resolution instead; the driver scales it to the paper.
const MAX_SHEET_BYTES: u64 = 640 * 1024 * 1024;

/// What a rendered sheet needs: the fewest bits that reproduce it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Depth {
    /// Only black and white (anti-aliased edges aside): 1 bit a pixel.
    Mono,
    /// Greys but no colour.
    Gray,
    Color,
}

/// Classify RGBA composed over white. A sheet with colour is `Color`. A grey sheet is `Mono`
/// unless a large share of it is mid-tones (a photograph, a big tinted area): `Mono` sheets are
/// dithered, so light grey text or a tinted box still come out, as the halftone the engine would
/// print anyway; `Gray` keeps 8 bits so the driver's own halftone handles the picture.
fn classify(rgba: &[u8], grayscale: bool) -> Depth {
    let mut mid = 0usize;
    let mut total = 0usize;
    let mut color = false;
    for px in rgba.as_chunks::<4>().0 {
        let (r, g, b) = (px[0], px[1], px[2]);
        if !grayscale && (r.abs_diff(g) > 8 || g.abs_diff(b) > 8) {
            color = true;
        }
        let y = (299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)) / 1000;
        if (24..232).contains(&y) {
            mid += 1;
        }
        total += 1;
    }
    if color {
        Depth::Color
    } else if total > 0 && mid.saturating_mul(1000) / total < MONO_MID_PERMILLE {
        Depth::Mono
    } else {
        Depth::Gray
    }
}

/// Mid-tone pixels per thousand above which a grey sheet keeps 8 bits. Measured at 600 dpi: a
/// page of 10–11 pt text has 17–22‰ anti-aliased edge pixels, the same page with a light grey
/// box on a twentieth of it 72‰, and a page with a photograph several hundred.
const MONO_MID_PERMILLE: usize = 120;

/// 8 × 8 Bayer ordered-dither matrix (thresholds 0–63): light greys become sparse dots, dark
/// greys dense ones, at the printer's own dot pitch.
const BAYER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// Premultiplied RGBA composed over opaque white, optionally as grey (Rec. 601 luma).
fn over_white(rgba: &mut [u8], grayscale: bool) {
    for px in rgba.as_chunks_mut::<4>().0 {
        let gap = 255 - u32::from(px[3]);
        let (r, g, b) = ((u32::from(px[0]) + gap).min(255), (u32::from(px[1]) + gap).min(255), (u32::from(px[2]) + gap).min(255));
        let (r, g, b) = if grayscale {
            let y = ((299 * r + 587 * g + 114 * b) / 1000).min(255);
            (y, y, y)
        } else {
            (r, g, b)
        };
        px[0] = r as u8;
        px[1] = g as u8;
        px[2] = b as u8;
        px[3] = 255;
    }
}

/// A device-independent bitmap ready for `StretchDIBits`: header (with palette) and pixels,
/// bottom-up, rows padded to four bytes.
struct Dib {
    /// `BITMAPINFOHEADER` followed by the palette, as one buffer.
    info: Vec<u8>,
    bits: Vec<u8>,
    depth: &'static str,
}

const BI_RGB: u32 = 0;
const BI_JPEG: u32 = 4;

fn header(width: u32, height: u32, bit_count: u16, compression: u32, size_image: u32, colors: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(40 + 4 * colors as usize);
    h.extend_from_slice(&40u32.to_le_bytes()); // biSize
    h.extend_from_slice(&(width as i32).to_le_bytes());
    h.extend_from_slice(&(height as i32).to_le_bytes()); // positive: bottom-up
    h.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    h.extend_from_slice(&bit_count.to_le_bytes());
    h.extend_from_slice(&compression.to_le_bytes());
    h.extend_from_slice(&size_image.to_le_bytes());
    h.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    h.extend_from_slice(&0i32.to_le_bytes());
    h.extend_from_slice(&colors.to_le_bytes()); // biClrUsed
    h.extend_from_slice(&colors.to_le_bytes()); // biClrImportant
    h
}

/// Rows of `rgba` (top-down, `width` pixels) packed with `pack` into `row_bytes`-wide rows,
/// bottom-up.
fn pack_rows(rgba: &[u8], width: usize, height: usize, row_bytes: usize, mut pack: impl FnMut(&[u8], &mut [u8])) -> Vec<u8> {
    let mut bits = vec![0u8; row_bytes * height];
    for (y, row) in rgba.chunks_exact(width * 4).take(height).enumerate() {
        let out_y = height - 1 - y;
        if let Some(out) = bits.get_mut(out_y * row_bytes..(out_y + 1) * row_bytes) {
            pack(row, out);
        }
    }
    bits
}

fn dib_mono(rgba: &[u8], width: u32, height: u32) -> Dib {
    let (w, h) = (width as usize, height as usize);
    let row_bytes = w.div_ceil(32) * 4;
    let mut y_row = 0usize;
    let bits = pack_rows(rgba, w, h, row_bytes, |row, out| {
        let bayer = &BAYER[y_row % 8];
        y_row += 1;
        for (x, px) in row.as_chunks::<4>().0.iter().enumerate() {
            let y = (299 * u32::from(px[0]) + 587 * u32::from(px[1]) + 114 * u32::from(px[2])) / 1000;
            // Luminance scaled to 0..=64 against the cell's threshold (0..=63): pure white
            // clears every cell, pure black none, and 50% grey half of them.
            if y * 64 / 255 > u32::from(bayer[x % 8])
                && let Some(b) = out.get_mut(x / 8)
            {
                *b |= 0x80 >> (x % 8);
            }
        }
    });
    let mut info = header(width, height, 1, BI_RGB, bits.len() as u32, 2);
    info.extend_from_slice(&[0, 0, 0, 0, 255, 255, 255, 0]); // palette: 0 = black, 1 = white
    Dib { info, bits, depth: "mono" }
}

fn dib_gray(rgba: &[u8], width: u32, height: u32) -> Dib {
    let (w, h) = (width as usize, height as usize);
    let row_bytes = (w + 3) & !3;
    let bits = pack_rows(rgba, w, h, row_bytes, |row, out| {
        for (px, o) in row.as_chunks::<4>().0.iter().zip(out.iter_mut()) {
            *o = ((299 * u32::from(px[0]) + 587 * u32::from(px[1]) + 114 * u32::from(px[2])) / 1000) as u8;
        }
    });
    let mut info = header(width, height, 8, BI_RGB, bits.len() as u32, 256);
    for i in 0..=255u8 {
        info.extend_from_slice(&[i, i, i, 0]);
    }
    Dib { info, bits, depth: "gray" }
}

fn dib_rgb(rgba: &[u8], width: u32, height: u32) -> Dib {
    let (w, h) = (width as usize, height as usize);
    let row_bytes = (w * 3 + 3) & !3;
    let bits = pack_rows(rgba, w, h, row_bytes, |row, out| {
        for (px, o) in row.as_chunks::<4>().0.iter().zip(out.chunks_exact_mut(3)) {
            o[0] = px[2];
            o[1] = px[1];
            o[2] = px[0];
        }
    });
    let info = header(width, height, 24, BI_RGB, bits.len() as u32, 0);
    Dib { info, bits, depth: "rgb" }
}

/// A JPEG the driver takes as is (`BI_JPEG`): PostScript drivers pass it through as `DCTDecode`.
fn dib_jpeg(rgba: &[u8], width: u32, height: u32, dpi: f64) -> Result<Dib, String> {
    use image::codecs::jpeg::{JpegEncoder, PixelDensity};
    let (w, h) = (width as usize, height as usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in rgba.as_chunks::<4>().0.iter().take(w * h) {
        rgb.extend_from_slice(&px[..3]);
    }
    let mut out = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    enc.set_pixel_density(PixelDensity::dpi(dpi.round().clamp(1.0, 65535.0) as u16));
    enc.encode(&rgb, width, height, image::ExtendedColorType::Rgb8).map_err(|e| format!("JPEG encoding failed: {e}"))?;
    let info = header(width, height, 0, BI_JPEG, out.len() as u32, 0);
    Ok(Dib { info, bits: out, depth: "jpeg" })
}

/// At the printer's resolution the compression artefacts are below what the engine can print.
const JPEG_QUALITY: u8 = 90;

#[cfg(windows)]
mod gdi {
    //! The Win32 side. Handles are closed in `Drop` so an error anywhere leaves nothing open,
    //! and an unfinished document is aborted rather than printed half-way.

    use std::ffi::c_void;

    use std::sync::Arc;

    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::Graphics::Gdi::{
        BITMAPINFO, CreateDCW, DEVMODE_COLLATE, DEVMODE_COLOR, DEVMODE_DUPLEX, DEVMODE_FIELD_FLAGS, DEVMODEW, DIB_RGB_COLORS, DeleteDC,
        GET_DEVICE_CAPS_INDEX, GetDeviceCaps, HDC, HORZRES, LOGPIXELSX, LOGPIXELSY, PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY, PHYSICALWIDTH,
        ResetDCW, SRCCOPY, StretchDIBits, VERTRES,
    };
    use windows::Win32::Graphics::Printing::{ClosePrinter, DocumentPropertiesW, GetDefaultPrinterW, OpenPrinterW, PRINTER_HANDLE};
    use windows::Win32::Storage::Xps::{
        AbortDoc, DC_COLLATE, DC_COPIES, DC_DUPLEX, DC_ORIENTATION, DC_PAPERNAMES, DC_PAPERS, DC_PAPERSIZE, DOCINFOW, DeviceCapabilitiesW, EndDoc,
        EndPage, ExtEscape, PRINTER_DEVICE_CAPABILITIES, StartDocW, StartPage,
    };
    use windows::core::{PCWSTR, PWSTR};

    use super::{Depth, Dib, Duplex, Job, classify, dib_gray, dib_jpeg, dib_mono, dib_rgb, over_white, sheet_sizes};
    use pdfcraft_render::{PageRenderer, RenderConfig};

    // DEVMODE field flags and values (wingdi.h).
    const DM_ORIENTATION: u32 = 0x0000_0001;
    const DM_PAPERSIZE: u32 = 0x0000_0002;
    const DM_COPIES: u32 = 0x0000_0100;
    const DM_COLOR: u32 = 0x0000_0800;
    const DM_DUPLEX: u32 = 0x0000_1000;
    const DM_COLLATE: u32 = 0x0000_8000;
    const DMORIENT_PORTRAIT: i16 = 1;
    const DMORIENT_LANDSCAPE: i16 = 2;
    const DMCOLOR_MONOCHROME: i16 = 1;
    const DMDUP_SIMPLEX: i16 = 1;
    const DMDUP_VERTICAL: i16 = 2; // long edge
    const DMDUP_HORIZONTAL: i16 = 3; // short edge
    const DM_OUT_BUFFER: u32 = 2;
    const DM_IN_BUFFER: u32 = 8;
    // Printer escapes.
    const QUERYESCSUPPORT: i32 = 8;
    const POSTSCRIPT_IDENTIFY: i32 = 4117;
    const CHECKJPEGFORMAT: i32 = 4119;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error(what: &str) -> String {
        // SAFETY: GetLastError has no preconditions.
        let code = unsafe { GetLastError() };
        let message = windows::core::Error::from(code).message();
        if message.is_empty() { format!("{what} failed (error {})", code.0) } else { format!("{what} failed: {}", message.trim_end()) }
    }

    /// The default printer's name, or `None` when there is none.
    fn default_printer() -> Option<String> {
        let mut len = 0u32;
        // SAFETY: a null buffer with len 0 asks for the needed length.
        let _ = unsafe { GetDefaultPrinterW(None, &mut len) };
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: buf holds `len` u16s, as told.
        let ok = unsafe { GetDefaultPrinterW(Some(PWSTR(buf.as_mut_ptr())), &mut len) };
        if !ok.as_bool() {
            return None;
        }
        let name: String = String::from_utf16_lossy(&buf).trim_end_matches('\0').to_owned();
        (!name.is_empty()).then_some(name)
    }

    /// An open printer handle.
    struct Printer {
        handle: PRINTER_HANDLE,
        name: Vec<u16>,
    }

    impl Printer {
        fn open(name: &str) -> Result<Self, String> {
            let wname = wide(name);
            let mut handle = PRINTER_HANDLE::default();
            // SAFETY: wname is NUL-terminated and outlives the call; handle is a valid out-pointer.
            unsafe { OpenPrinterW(PCWSTR(wname.as_ptr()), &mut handle, None) }
                .map_err(|_| format!("the printer is not available: {name} (reopen Print to refresh the list)"))?;
            Ok(Printer { handle, name: wname })
        }

        /// The driver's default DEVMODE as a byte buffer (its size is the driver's to decide).
        fn default_devmode(&self) -> Result<DevMode, String> {
            // SAFETY: fmode 0 asks for the size; no buffers are written.
            let size = unsafe { DocumentPropertiesW(None, self.handle, PCWSTR(self.name.as_ptr()), None, None, 0) };
            if size <= 0 {
                return Err(last_error("reading the printer's settings"));
            }
            let mut dm = DevMode::zeroed(size as usize);
            // SAFETY: the buffer is at least `size` bytes, as the driver asked.
            let r = unsafe { DocumentPropertiesW(None, self.handle, PCWSTR(self.name.as_ptr()), Some(dm.as_mut_ptr()), None, DM_OUT_BUFFER) };
            if r < 0 {
                return Err(last_error("reading the printer's settings"));
            }
            Ok(dm)
        }

        /// Let the driver validate (and adjust) a DEVMODE the job changed.
        fn validate(&self, dm: &mut DevMode) -> Result<(), String> {
            let input = dm.clone();
            // SAFETY: both buffers are full DEVMODEs of the driver's own size.
            let r = unsafe {
                DocumentPropertiesW(
                    None,
                    self.handle,
                    PCWSTR(self.name.as_ptr()),
                    Some(dm.as_mut_ptr()),
                    Some(input.as_ptr()),
                    DM_IN_BUFFER | DM_OUT_BUFFER,
                )
            };
            if r < 0 { Err(last_error("applying the print settings")) } else { Ok(()) }
        }

        fn capability(&self, what: PRINTER_DEVICE_CAPABILITIES, out: Option<&mut [u16]>) -> i32 {
            let ptr = out.map(|o| PWSTR(o.as_mut_ptr()));
            // SAFETY: the output buffer, when given, is sized by the caller from a previous count.
            unsafe { DeviceCapabilitiesW(PCWSTR(self.name.as_ptr()), PCWSTR::null(), what, ptr, None) }
        }

        /// The driver's papers: (id, width, height) in tenths of a millimetre, portrait.
        fn papers(&self) -> Vec<(i16, i32, i32)> {
            let n = self.capability(DC_PAPERS, None);
            if n <= 0 || n > 4096 {
                return Vec::new();
            }
            let n = n as usize;
            let mut ids = vec![0u16; n];
            let mut sizes = vec![0u16; n * 4]; // POINT (two i32) each
            if self.capability(DC_PAPERS, Some(&mut ids)) as usize != n || self.capability(DC_PAPERSIZE, Some(&mut sizes)) as usize != n {
                return Vec::new();
            }
            ids.iter()
                .zip(sizes.chunks_exact(4))
                .map(|(&id, p)| (id as i16, i32::from(p[0]) | (i32::from(p[1]) << 16), i32::from(p[2]) | (i32::from(p[3]) << 16)))
                .collect()
        }

        fn paper_name(&self, index: usize) -> Option<String> {
            let n = self.capability(DC_PAPERNAMES, None);
            if n <= 0 || index >= n as usize {
                return None;
            }
            let mut names = vec![0u16; n as usize * 64];
            if self.capability(DC_PAPERNAMES, Some(&mut names)) <= 0 {
                return None;
            }
            names.get(index * 64..(index + 1) * 64).map(|s| String::from_utf16_lossy(s).trim_end_matches('\0').to_owned())
        }
    }

    impl Drop for Printer {
        fn drop(&mut self) {
            // SAFETY: the handle came from OpenPrinterW and is closed once.
            let _ = unsafe { ClosePrinter(self.handle) };
        }
    }

    /// A DEVMODEW of the driver's size (the public struct plus driver-private bytes).
    #[derive(Clone)]
    struct DevMode(Vec<u64>); // u64 for alignment

    impl DevMode {
        fn zeroed(bytes: usize) -> Self {
            DevMode(vec![0u64; bytes.div_ceil(8).max(std::mem::size_of::<DEVMODEW>().div_ceil(8))])
        }
        fn as_ptr(&self) -> *const DEVMODEW {
            self.0.as_ptr().cast()
        }
        fn as_mut_ptr(&mut self) -> *mut DEVMODEW {
            self.0.as_mut_ptr().cast()
        }
        fn get_mut(&mut self) -> &mut DEVMODEW {
            // SAFETY: the buffer is at least a DEVMODEW long and 8-byte aligned.
            unsafe { &mut *self.as_mut_ptr() }
        }
        fn set_orientation(&mut self, landscape: bool) {
            let dm = self.get_mut();
            dm.Anonymous1.Anonymous1.dmOrientation = if landscape { DMORIENT_LANDSCAPE } else { DMORIENT_PORTRAIT };
            dm.dmFields |= DEVMODE_FIELD_FLAGS(DM_ORIENTATION);
        }
    }

    /// A printer device context with a document in progress.
    struct Document {
        hdc: HDC,
        started: bool,
    }

    impl Document {
        fn open(printer: &Printer, dm: &DevMode) -> Result<Self, String> {
            let driver = wide("WINSPOOL");
            // SAFETY: the strings are NUL-terminated; the DEVMODE is a validated driver buffer.
            let hdc = unsafe { CreateDCW(PCWSTR(driver.as_ptr()), PCWSTR(printer.name.as_ptr()), PCWSTR::null(), Some(dm.as_ptr())) };
            if hdc.is_invalid() {
                return Err(last_error("opening the printer"));
            }
            Ok(Document { hdc, started: false })
        }

        fn cap(&self, index: GET_DEVICE_CAPS_INDEX) -> i32 {
            // SAFETY: hdc is a live device context.
            unsafe { GetDeviceCaps(Some(self.hdc), index) }
        }

        /// Whether the driver supports a printer escape.
        fn supports_escape(&self, escape: i32) -> bool {
            let input = escape.to_le_bytes();
            // SAFETY: QUERYESCSUPPORT reads one i32 from the input buffer.
            unsafe { ExtEscape(self.hdc, QUERYESCSUPPORT, Some(&input), None) > 0 }
        }

        fn start(&mut self, title: &str) -> Result<(), String> {
            let wtitle = wide(title);
            let info = DOCINFOW {
                cbSize: std::mem::size_of::<DOCINFOW>() as i32,
                lpszDocName: PCWSTR(wtitle.as_ptr()),
                lpszOutput: PCWSTR::null(),
                lpszDatatype: PCWSTR::null(),
                fwType: 0,
            };
            // SAFETY: info is fully initialised and its strings outlive the call.
            if unsafe { StartDocW(self.hdc, &info) } <= 0 {
                return Err(last_error("starting the print job"));
            }
            self.started = true;
            Ok(())
        }

        /// Change orientation (or other DEVMODE settings) between pages.
        fn reset(&mut self, dm: &DevMode) -> Result<(), String> {
            // SAFETY: dm is a validated DEVMODE of the driver's size.
            let hdc = unsafe { ResetDCW(self.hdc, dm.as_ptr()) };
            if hdc.is_invalid() {
                return Err(last_error("turning the page"));
            }
            self.hdc = hdc;
            Ok(())
        }

        fn page(&mut self, dib: &Dib, dest: (i32, i32, i32, i32), src: (u32, u32)) -> Result<(), String> {
            // SAFETY: hdc has a document started.
            if unsafe { StartPage(self.hdc) } <= 0 {
                return Err(last_error("starting a page"));
            }
            let (x, y, w, h) = dest;
            // SAFETY: `info` starts with a BITMAPINFOHEADER followed by its palette, and `bits`
            // holds the image it describes.
            let lines = unsafe {
                StretchDIBits(
                    self.hdc,
                    x,
                    y,
                    w,
                    h,
                    0,
                    0,
                    src.0 as i32,
                    src.1 as i32,
                    Some(dib.bits.as_ptr().cast::<c_void>()),
                    dib.info.as_ptr().cast::<BITMAPINFO>(),
                    DIB_RGB_COLORS,
                    SRCCOPY,
                )
            };
            // GDI_ERROR is 0xFFFF_FFFF as an i32: -1. Zero rows is a failure too.
            if lines <= 0 {
                let e = last_error("drawing the page");
                // SAFETY: a page was started.
                unsafe { EndPage(self.hdc) };
                return Err(e);
            }
            // SAFETY: a page was started.
            if unsafe { EndPage(self.hdc) } <= 0 {
                return Err(last_error("finishing a page"));
            }
            Ok(())
        }

        fn finish(mut self) -> Result<(), String> {
            // SAFETY: a document was started.
            let r = unsafe { EndDoc(self.hdc) };
            self.started = false;
            if r <= 0 { Err(last_error("finishing the print job")) } else { Ok(()) }
        }
    }

    impl Drop for Document {
        fn drop(&mut self) {
            // SAFETY: hdc is live; AbortDoc discards an unfinished job, DeleteDC closes the context.
            unsafe {
                if self.started {
                    AbortDoc(self.hdc);
                }
                let _ = DeleteDC(self.hdc);
            }
        }
    }

    /// Diagnostics: with `DOCUVIEW_PRINT_DUMP=<folder>` set, every sheet is also written there as
    /// it goes to the driver (`sheet-N.<depth>.png`, or `.jpg` for a JPEG), so the exact output
    /// can be looked at without a printer.
    fn dump_sheet(dib: &Dib, rgba: &[u8], width: u32, height: u32, index: usize) {
        let Some(dir) = std::env::var_os("DOCUVIEW_PRINT_DUMP") else { return };
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let n = index + 1;
        let written = match dib.depth {
            "jpeg" => std::fs::write(dir.join(format!("sheet-{n}.jpeg.jpg")), &dib.bits),
            depth => {
                // Unpack the DIB rows (bottom-up) back into a top-down grey image of what was sent.
                let (w, h) = (width as usize, height as usize);
                let mut grey = vec![0u8; w * h];
                match depth {
                    "mono" => {
                        let row_bytes = w.div_ceil(32) * 4;
                        for y in 0..h {
                            let row = &dib.bits[(h - 1 - y) * row_bytes..];
                            for x in 0..w {
                                grey[y * w + x] = if row[x / 8] & (0x80 >> (x % 8)) != 0 { 255 } else { 0 };
                            }
                        }
                    }
                    "gray" => {
                        let row_bytes = (w + 3) & !3;
                        for y in 0..h {
                            grey[y * w..(y + 1) * w].copy_from_slice(&dib.bits[(h - 1 - y) * row_bytes..][..w]);
                        }
                    }
                    _ => {
                        for (g, px) in grey.iter_mut().zip(rgba.chunks_exact(4)) {
                            *g = ((299 * u32::from(px[0]) + 587 * u32::from(px[1]) + 114 * u32::from(px[2])) / 1000) as u8;
                        }
                    }
                }
                let mut out = Vec::new();
                let enc = image::codecs::png::PngEncoder::new(&mut out);
                match image::ImageEncoder::write_image(enc, &grey, width, height, image::ExtendedColorType::L8) {
                    Ok(()) => std::fs::write(dir.join(format!("sheet-{n}.{depth}.png")), &out),
                    Err(e) => Err(std::io::Error::other(e.to_string())),
                }
            }
        };
        if let Err(e) = written {
            eprintln!("DOCUVIEW_PRINT_DUMP: sheet {n} not written: {e}");
        }
    }

    /// Points to tenths of a millimetre.
    fn tenth_mm(pt: f32) -> i32 {
        (f64::from(pt) * 25.4 / 72.0 * 10.0).round() as i32
    }

    /// Spool the print-ready PDF: see the crate docs.
    pub fn submit_with_progress(pdf: &[u8], job: &Job, progress: &mut dyn FnMut(usize, usize)) -> Result<String, String> {
        let bytes = Arc::new(pdf.to_vec());
        let sizes = sheet_sizes(&bytes)?;
        let count = sizes.len();
        let first = sizes.first().copied().unwrap_or((612.0, 792.0));
        // Exact size: every sheet must be the first one's size, either way round (as upstream).
        if !job.shrink_to_fit {
            let same = |a: f32, b: f32| (a - b).abs() <= 1.0;
            if let Some((i, s)) =
                sizes.iter().enumerate().find(|(_, s)| !(same(s.0, first.0) && same(s.1, first.1) || same(s.0, first.1) && same(s.1, first.0)))
            {
                return Err(format!(
                    "sheet {} is {:.2} × {:.2} inches but sheet 1 is {:.2} × {:.2}: choose shrink-to-fit, or print them separately",
                    i + 1,
                    s.0 / 72.0,
                    s.1 / 72.0,
                    first.0 / 72.0,
                    first.1 / 72.0
                ));
            }
        }

        let name = match job.printer {
            Some(n) => n.to_owned(),
            None => default_printer().ok_or("there is no default printer; choose a printer")?,
        };
        let printer = Printer::open(&name)?;
        let mut dm = printer.default_devmode()?;

        // Paper: the driver's own size for the first sheet (portrait dimensions), within a millimetre.
        let (short, long) = (tenth_mm(first.0.min(first.1)), tenth_mm(first.0.max(first.1)));
        let papers = printer.papers();
        let paper = papers.iter().position(|&(_, w, h)| (w - short).abs() <= 10 && (h - long).abs() <= 10);
        match (paper, job.shrink_to_fit) {
            (Some(i), _) => {
                if let Some(&(id, _, _)) = papers.get(i) {
                    let d = dm.get_mut();
                    d.Anonymous1.Anonymous1.dmPaperSize = id;
                    d.dmFields |= DEVMODE_FIELD_FLAGS(DM_PAPERSIZE);
                    let _ = printer.paper_name(i);
                }
            }
            (None, true) => {} // the driver's default paper; sheets are fitted to it below
            (None, false) => {
                return Err(format!(
                    "{name} has no {:.2} × {:.2} inch paper; choose a paper size it has, or add this one in its driver",
                    first.0 / 72.0,
                    first.1 / 72.0
                ));
            }
        }
        let first_landscape = first.0 > first.1 + 1.0;
        if printer.capability(DC_ORIENTATION, None) != 0 {
            dm.set_orientation(first_landscape);
        }
        // Copies and collation: by the driver when it can, else by this loop.
        let driver_copies = printer.capability(DC_COPIES, None).max(1) as u32;
        let copies = job.copies.clamp(1, 999);
        let (dm_copies, our_copies) = if copies <= driver_copies { (copies, 1) } else { (1, copies) };
        {
            let d = dm.get_mut();
            d.Anonymous1.Anonymous1.dmCopies = dm_copies as i16;
            d.dmFields |= DEVMODE_FIELD_FLAGS(DM_COPIES);
            if dm_copies > 1 && printer.capability(DC_COLLATE, None) > 0 {
                d.dmCollate = DEVMODE_COLLATE(if job.collate { 1 } else { 0 });
                d.dmFields |= DEVMODE_FIELD_FLAGS(DM_COLLATE);
            }
            if job.duplex != Duplex::Default && printer.capability(DC_DUPLEX, None) > 0 {
                d.dmDuplex = DEVMODE_DUPLEX(match job.duplex {
                    Duplex::LongEdge => DMDUP_VERTICAL,
                    Duplex::ShortEdge => DMDUP_HORIZONTAL,
                    Duplex::Off | Duplex::Default => DMDUP_SIMPLEX,
                });
                d.dmFields |= DEVMODE_FIELD_FLAGS(DM_DUPLEX);
            }
            if job.grayscale {
                d.dmColor = DEVMODE_COLOR(DMCOLOR_MONOCHROME);
                d.dmFields |= DEVMODE_FIELD_FLAGS(DM_COLOR);
            }
        }
        printer.validate(&mut dm)?;

        let mut doc = Document::open(&printer, &dm)?;
        let (dpi_x, dpi_y) = (doc.cap(LOGPIXELSX).max(1), doc.cap(LOGPIXELSY).max(1));
        let postscript = doc.supports_escape(POSTSCRIPT_IDENTIFY);
        let jpeg = postscript && doc.supports_escape(CHECKJPEGFORMAT);
        let mut renderer = PageRenderer::new(Arc::clone(&bytes), RenderConfig::default());
        doc.start(job.title)?;
        progress(0, count);

        let mut landscape = first_landscape;
        let mut sent = 0usize;
        let order: Vec<usize> = if job.collate || our_copies == 1 {
            (0..our_copies as usize).flat_map(|_| 0..count).collect()
        } else {
            (0..count).flat_map(|i| std::iter::repeat_n(i, our_copies as usize)).collect()
        };
        for &i in &order {
            let (w_pt, h_pt) = sizes.get(i).copied().unwrap_or(first);
            let this_landscape = w_pt > h_pt + 1.0;
            if this_landscape != landscape && printer.capability(DC_ORIENTATION, None) != 0 {
                dm.set_orientation(this_landscape);
                printer.validate(&mut dm)?;
                doc.reset(&dm)?;
                landscape = this_landscape;
            }
            // The paper, in device pixels: printable area and the physical sheet.
            let (res_w, res_h) = (doc.cap(HORZRES).max(1), doc.cap(VERTRES).max(1));
            let (phys_w, phys_h) = (doc.cap(PHYSICALWIDTH).max(res_w), doc.cap(PHYSICALHEIGHT).max(res_h));
            let (off_x, off_y) = (doc.cap(PHYSICALOFFSETX).max(0), doc.cap(PHYSICALOFFSETY).max(0));
            let sheet_w = (f64::from(w_pt) * f64::from(dpi_x) / 72.0).round() as i32;
            let sheet_h = (f64::from(h_pt) * f64::from(dpi_y) / 72.0).round() as i32;
            let (dest_w, dest_h, x, y) = if job.shrink_to_fit {
                let scale = if sheet_w > res_w || sheet_h > res_h {
                    (f64::from(res_w) / f64::from(sheet_w.max(1))).min(f64::from(res_h) / f64::from(sheet_h.max(1)))
                } else {
                    1.0
                };
                let (dw, dh) = ((f64::from(sheet_w) * scale).round() as i32, (f64::from(sheet_h) * scale).round() as i32);
                // Centred on the physical sheet, in printable-area coordinates.
                ((dw.max(1)), (dh.max(1)), (phys_w - dw) / 2 - off_x, (phys_h - dh) / 2 - off_y)
            } else {
                (sheet_w.max(1), sheet_h.max(1), -off_x, -off_y)
            };
            // Render at the printer's resolution: one device pixel per printer dot.
            let scale = (f64::from(dest_w) / f64::from(w_pt.max(1.0))) as f32;
            let (rw, rh, mut rgba) = super::render_sheet(&mut renderer, i, w_pt, h_pt, scale)?;
            over_white(&mut rgba, job.grayscale);
            let dib = match classify(&rgba, job.grayscale) {
                Depth::Mono => dib_mono(&rgba, rw, rh),
                Depth::Gray => dib_gray(&rgba, rw, rh),
                Depth::Color if jpeg => dib_jpeg(&rgba, rw, rh, f64::from(dpi_x))?,
                Depth::Color => dib_rgb(&rgba, rw, rh),
            };
            dump_sheet(&dib, &rgba, rw, rh, i);
            drop(rgba);
            doc.page(&dib, (x, y, dest_w, dest_h), (rw, rh))?;
            sent += 1;
            progress(sent.min(count), count);
        }
        doc.finish()?;
        Ok(format!("spooled {count} sheet(s)"))
    }
}

#[cfg(windows)]
pub use gdi::submit_with_progress;

#[cfg(not(windows))]
pub fn submit_with_progress(_pdf: &[u8], _job: &Job, _progress: &mut dyn FnMut(usize, usize)) -> Result<String, String> {
    Err("printing through GDI is Windows only".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(pixels: &[[u8; 3]]) -> Vec<u8> {
        pixels.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect()
    }

    #[test]
    fn text_is_mono_grey_fills_are_grey_and_colour_is_colour() {
        let mut text = vec![[255, 255, 255]; 1000];
        text[..40].fill([0, 0, 0]);
        text[40..60].fill([128, 128, 128]); // 2% anti-aliased edges
        assert_eq!(classify(&page(&text), false), Depth::Mono);
        let mut shaded = text.clone();
        shaded[100..170].fill([230, 230, 230]); // a light grey box on 7% of the page: still dithered mono
        assert_eq!(classify(&page(&shaded), false), Depth::Mono);
        let mut photo = text.clone();
        photo[100..400].fill([128, 128, 128]); // a grey picture on 30% of the page
        assert_eq!(classify(&page(&photo), false), Depth::Gray);
        let mut colour = text.clone();
        colour[0] = [31, 95, 191];
        assert_eq!(classify(&page(&colour), false), Depth::Color);
        assert_eq!(classify(&page(&colour), true), Depth::Mono, "a greyscale job never sends colour");
    }

    #[test]
    fn dibs_have_padded_bottom_up_rows() {
        let mut rgba = page(&[[0, 0, 0], [255, 255, 255], [255, 255, 255]]);
        rgba.extend(page(&[[255, 255, 255], [255, 255, 255], [0, 0, 0]])); // 3 x 2
        let mono = dib_mono(&rgba, 3, 2);
        assert_eq!(mono.bits.len(), 8, "two rows of four bytes");
        assert_eq!(mono.bits[0], 0b1100_0000, "bottom row first: white, white, black");
        assert_eq!(mono.bits[4], 0b0110_0000, "top row: black, white, white");
        // Pure white and pure black stay solid; a flat 50% grey dithers to half dots.
        let dots = |level: u8| -> u32 { dib_mono(&page(&[[level; 3]; 64]), 8, 8).bits.iter().map(|b| b.count_ones()).sum() };
        assert_eq!(dots(255), 64, "white has no dots");
        assert_eq!(dots(0), 0, "black is solid");
        assert!((28..=36).contains(&dots(128)), "{} white cells of 64 at 50% grey", dots(128));
        assert!((54..=62).contains(&dots(230)), "{} white cells of 64 for a light grey box", dots(230));
        let gray = dib_gray(&rgba, 3, 2);
        assert_eq!(&gray.bits[..4], &[255, 255, 0, 0]);
        let rgb = dib_rgb(&rgba, 3, 2);
        assert_eq!(rgb.bits.len(), 24, "rows of 9 bytes padded to 12");
        assert_eq!(&rgb.bits[12..15], &[0, 0, 0]);
    }

    /// A one-page PDF of `w` x `h` points with a filled black rectangle at `rect` (x, y, w, h,
    /// PDF user space, origin bottom-left).
    fn pdf_with_rect(w: f32, h: f32, rect: [f32; 4]) -> Vec<u8> {
        let content = format!("0 0 0 rg {} {} {} {} re f", rect[0], rect[1], rect[2], rect[3]);
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents 4 0 R >>"),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    #[test]
    fn oversized_sheets_render_at_full_resolution_in_tiles_without_seams() {
        // Tabloid at 600 dpi: 6600 x 10200, beyond the renderer's 8192-px side cap, so tiled
        // (two columns, three rows). The square straddles both tile seams.
        let pdf = Arc::new(pdf_with_rect(792.0, 1224.0, [400.0, 624.0, 200.0, 200.0]));
        let mut renderer = PageRenderer::new(Arc::clone(&pdf), pdfcraft_render::RenderConfig::default());
        let scale = 600.0 / 72.0;
        let (w, h, rgba) = render_sheet(&mut renderer, 0, 792.0, 1224.0, scale).unwrap();
        assert_eq!((w, h), (6600, 10200));
        let px = |x: u32, y: u32| -> [u8; 3] {
            let i = ((y * w + x) * 4) as usize;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        };
        // PDF y is bottom-up: the square spans x 400-600 pt and, from the top, y 400-600 pt.
        let (x0, x1) = ((400.0 * scale) as u32, (600.0 * scale) as u32);
        let (y0, y1) = ((400.0 * scale) as u32, (600.0 * scale) as u32);
        assert!(x0 < 4096 && x1 > 4096 && y0 < 4096 && y1 > 4096, "the square must cross the seams at 4096");
        for (x, y) in [(x0 + 5, y0 + 5), (4095, y0 + 5), (4096, y0 + 5), (x0 + 5, 4095), (x0 + 5, 4096), (4096, 4096), (x1 - 5, y1 - 5)] {
            assert_eq!(px(x, y), [0, 0, 0], "black inside the square at {x}, {y}");
        }
        for (x, y) in [(x0 - 5, y0 + 5), (x1 + 5, y1 - 5), (4096, y0 - 5), (4096, y1 + 5), (10, 10), (w - 10, h - 10)] {
            assert_eq!(px(x, y), [255, 255, 255], "white outside the square at {x}, {y}");
        }
        // A sheet within the caps renders whole, at the asked scale.
        let small = Arc::new(pdf_with_rect(612.0, 792.0, [0.0, 0.0, 10.0, 10.0]));
        let mut renderer = PageRenderer::new(small, pdfcraft_render::RenderConfig::default());
        let (w, h, _) = render_sheet(&mut renderer, 0, 612.0, 792.0, scale).unwrap();
        assert_eq!((w, h), (5100, 6600));
    }

    #[test]
    fn over_white_composes_and_greys() {
        let mut px = vec![0, 0, 0, 0, 255, 0, 0, 255];
        over_white(&mut px, false);
        assert_eq!(px, [255, 255, 255, 255, 255, 0, 0, 255]);
        over_white(&mut px, true);
        assert_eq!(&px[4..7], &[76, 76, 76]);
    }
}
