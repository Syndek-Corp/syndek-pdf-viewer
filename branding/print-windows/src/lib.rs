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
//! - black-and-white sheets (text, line art) as 1-bit at the printer's resolution, ordered-dithered
//!   so light grey text and tints survive as the halftone the engine prints anyway: 0.3–1 MB a sheet;
//! - grey sheets (photographs, large tints) as 8-bit grey;
//! - colour sheets as JPEG when the driver is a PostScript one (it passes JPEG through as
//!   `DCTDecode`, ~3 MB a sheet) and as 24-bit otherwise (PCL drivers compress on their own).
//!
//! Sheets render at the printer's resolution up to [`MAX_RENDER_DPI`]. A sheet that fits in
//! [`WHOLE_SHEET_BYTES`] is rendered whole (Letter at 600 dpi); a larger one (Legal, Tabloid, a
//! plotter sheet, or a 1200 dpi driver) is rendered and sent in bands of [`BAND_ROWS`] rows, so
//! paper size never lowers the resolution or exhausts memory. The next sheet renders on a worker
//! thread while the current one is encoded and sent.
//!
//! Copies, collation, two-sided, colour, paper and orientation go through `DEVMODE`, validated by
//! the driver (`DocumentProperties`); a size the driver doesn't list is asked for as a custom size
//! when the driver takes those. Sheets of the other orientation turn the job with `ResetDC`
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

/// Sheets never render above this: a 1200 dpi driver gets 600 dpi images scaled 2x, which prints
/// the same as a 600 dpi engine would, at a quarter of the time and memory.
const MAX_RENDER_DPI: f32 = 600.0;

/// A sheet whose RGBA render is at most this big is rendered whole (and classified from the whole
/// sheet); bigger ones are sent in bands. Letter at 600 dpi is 135 MB.
const WHOLE_SHEET_BYTES: u64 = 160 * 1024 * 1024;

/// Rows per band when a sheet is streamed.
const BAND_ROWS: u32 = 1024;

/// The renderer draws a tile of at most this many pixels a side.
const TILE: u32 = 4096;

/// A streamed sheet is classified (colour, grey or black-and-white) from a render at this
/// resolution, made before the bands.
const PROBE_DPI: f32 = 200.0;

/// Mid-tone pixels per thousand above which a grey sheet keeps 8 bits, measured at the printer's
/// resolution (600 dpi): a page of 10–11 pt text has 17–22‰ anti-aliased edge pixels, the same
/// page with a light grey box on a twentieth of it 72‰, and a page with a photograph several
/// hundred.
const MONO_MID_PERMILLE: usize = 120;

/// The same limit for a [`PROBE_DPI`] render, where text edges take three times the share.
const PROBE_MONO_MID_PERMILLE: usize = 250;

/// A sheet's size in points (after rotation), as the print-ready PDF displays it.
fn sheet_sizes(pdf: &Arc<Vec<u8>>) -> Result<Vec<(f32, f32)>, String> {
    let info = pdfcraft_render::inspect(Arc::clone(pdf), None).map_err(|e| format!("the print-ready PDF can't be read: {e}"))?;
    let sizes: Vec<(f32, f32)> = info.pages.iter().map(|p| (p.width, p.height)).collect();
    if sizes.is_empty() {
        return Err("the print-ready PDF has no pages".into());
    }
    Ok(sizes)
}

/// Render sheet `page` whole at `scale` (capped by the renderer's limits: 8192 px a side, 64 M
/// pixels): `(width, height, premultiplied RGBA)`.
fn render_whole(renderer: &mut PageRenderer, page: usize, w_pt: f32, h_pt: f32, scale: f32) -> Result<(u32, u32, Vec<u8>), String> {
    let n = page + 1;
    let capped = effective_scale(w_pt, h_pt, scale);
    let r = renderer.render(RenderRequest { page, scale: capped, tag: PRINT_TAG, ..Default::default() });
    if let Some(e) = r.error {
        return Err(format!("sheet {n} could not be rendered: {e}"));
    }
    if r.width == 0 || r.height == 0 {
        return Err(format!("sheet {n} rendered empty"));
    }
    Ok((r.width, r.height, r.rgba.to_vec()))
}

/// Render rows `y..y + rows` of sheet `page` at `scale` (uncapped: tiles may be asked for at any
/// scale), `full_w` pixels wide, as premultiplied RGBA.
fn render_band(renderer: &mut PageRenderer, page: usize, scale: f32, full_w: u32, y: u32, rows: u32) -> Result<Vec<u8>, String> {
    let n = page + 1;
    let (fw, rows_u) = (full_w as usize, rows as usize);
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(fw * rows_u * 4).map_err(|_| format!("sheet {n}: not enough memory for a {full_w} x {rows} band"))?;
    rgba.resize(fw * rows_u * 4, 0);
    for ty in (0..rows).step_by(TILE as usize) {
        let th = TILE.min(rows - ty);
        for tx in (0..full_w).step_by(TILE as usize) {
            let tw = TILE.min(full_w - tx);
            let tile = Tile { x: tx, y: y + ty, w: tw, h: th };
            let r = renderer.render(RenderRequest { page, scale, tile: Some(tile), tag: PRINT_TAG, ..Default::default() });
            if let Some(e) = r.error {
                return Err(format!("sheet {n} could not be rendered (tile at {tx}, {}): {e}", y + ty));
            }
            let (rw, rh) = (r.width as usize, r.height as usize);
            if rw < tw as usize || rh < th as usize {
                return Err(format!("sheet {n}: tile at {tx}, {} came back {rw} x {rh}, not {tw} x {th}", y + ty));
            }
            let src = r.rgba.to_vec();
            for row in 0..th as usize {
                let out_row = ty as usize + row;
                let (Some(from), Some(to)) = (
                    src.get(row * rw * 4..(row * rw + tw as usize) * 4),
                    rgba.get_mut((out_row * fw + tx as usize) * 4..(out_row * fw + tx as usize + tw as usize) * 4),
                ) else {
                    return Err(format!("sheet {n}: tile at {tx}, {} is out of bounds", y + ty));
                };
                to.copy_from_slice(from);
            }
        }
    }
    Ok(rgba)
}

/// Render sheet `page` at `scale` device pixels per point as one image: whole when the renderer
/// can draw it in one go, else assembled from bands at the full scale, so no sheet is sent below
/// the printer's resolution.
fn render_sheet(renderer: &mut PageRenderer, page: usize, w_pt: f32, h_pt: f32, scale: f32) -> Result<(u32, u32, Vec<u8>), String> {
    let capped = effective_scale(w_pt, h_pt, scale);
    let (full_w, full_h) = (device_pixels(w_pt, scale), device_pixels(h_pt, scale));
    if capped >= scale * 0.999 || full_w == 0 || full_h == 0 {
        return render_whole(renderer, page, w_pt, h_pt, scale);
    }
    let n = page + 1;
    let bytes = full_w as usize * full_h as usize * 4;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(bytes).map_err(|_| format!("sheet {n}: not enough memory for a {full_w} x {full_h} render"))?;
    for y in (0..full_h).step_by(TILE as usize) {
        let rows = TILE.min(full_h - y);
        rgba.extend_from_slice(&render_band(renderer, page, scale, full_w, y, rows)?);
    }
    Ok((full_w, full_h, rgba))
}

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
/// unless more than `mono_limit` per thousand of it is mid-tones (a photograph, a big tinted
/// area): `Mono` sheets are dithered, so light grey text or a tinted box still come out, as the
/// halftone the engine would print anyway; `Gray` keeps 8 bits so the driver's own halftone
/// handles the picture.
fn classify(rgba: &[u8], grayscale: bool, mono_limit: usize) -> Depth {
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
    } else if total > 0 && mid.saturating_mul(1000) / total < mono_limit {
        Depth::Mono
    } else {
        Depth::Gray
    }
}

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

/// Rows of `rgba` (top-down, `width` pixels) packed with `pack(row_index, row, out)` into
/// `row_bytes`-wide rows, bottom-up.
fn pack_rows(rgba: &[u8], width: usize, height: usize, row_bytes: usize, mut pack: impl FnMut(usize, &[u8], &mut [u8])) -> Vec<u8> {
    let mut bits = vec![0u8; row_bytes * height];
    for (y, row) in rgba.chunks_exact(width * 4).take(height).enumerate() {
        let out_y = height - 1 - y;
        if let Some(out) = bits.get_mut(out_y * row_bytes..(out_y + 1) * row_bytes) {
            pack(y, row, out);
        }
    }
    bits
}

/// 1-bit, ordered-dithered. `y_offset` is the image's first row on the sheet, so bands of one
/// sheet share the dither pattern's phase and join without a seam.
fn dib_mono(rgba: &[u8], width: u32, height: u32, y_offset: usize) -> Dib {
    let (w, h) = (width as usize, height as usize);
    let row_bytes = w.div_ceil(32) * 4;
    let bits = pack_rows(rgba, w, h, row_bytes, |y, row, out| {
        let bayer = &BAYER[(y_offset + y) % 8];
        for (x, px) in row.as_chunks::<4>().0.iter().enumerate() {
            let lum = (299 * u32::from(px[0]) + 587 * u32::from(px[1]) + 114 * u32::from(px[2])) / 1000;
            // Luminance scaled to 0..=64 against the cell's threshold (0..=63): pure white
            // clears every cell, pure black none, and 50% grey half of them.
            if lum * 64 / 255 > u32::from(bayer[x % 8])
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
    let bits = pack_rows(rgba, w, h, row_bytes, |_, row, out| {
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
    let bits = pack_rows(rgba, w, h, row_bytes, |_, row, out| {
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

/// Encode an image (RGBA over white) at `depth`; `y_offset` is its first row on the sheet.
fn encode(rgba: &[u8], width: u32, height: u32, depth: Depth, jpeg: bool, dpi: f64, y_offset: usize) -> Result<Dib, String> {
    Ok(match depth {
        Depth::Mono => dib_mono(rgba, width, height, y_offset),
        Depth::Gray => dib_gray(rgba, width, height),
        Depth::Color if jpeg => dib_jpeg(rgba, width, height, dpi)?,
        Depth::Color => dib_rgb(rgba, width, height),
    })
}

/// How one sheet of the job is placed and rendered. Positions are device pixels in the printable
/// area's coordinates (the origin is the top-left of the printable area).
#[derive(Clone, Copy, Debug)]
struct SheetPlan {
    index: usize,
    landscape: bool,
    x: i32,
    y: i32,
    dest_w: i32,
    dest_h: i32,
    /// Render scale, device pixels per point (at most [`MAX_RENDER_DPI`]).
    scale: f32,
}

/// What the render worker hands the sending thread, in order.
enum Msg {
    /// A whole sheet.
    Whole {
        w: u32,
        h: u32,
        rgba: Vec<u8>,
    },
    /// A streamed sheet begins: its full size and a small render to classify it from.
    Begin {
        w: u32,
        h: u32,
        probe_w: u32,
        probe_h: u32,
        probe: Vec<u8>,
    },
    Band {
        y: u32,
        rows: u32,
        rgba: Vec<u8>,
    },
    End,
    Error(String),
}

/// The render thread: sheets in `plan` order, whole when they fit [`WHOLE_SHEET_BYTES`], else
/// a probe and then bands. Stops when the receiver is gone.
fn render_worker(bytes: Arc<Vec<u8>>, sizes: Vec<(f32, f32)>, plan: Vec<SheetPlan>, tx: std::sync::mpsc::SyncSender<Msg>) {
    let mut renderer = PageRenderer::new(bytes, pdfcraft_render::RenderConfig::default());
    let send = |m: Msg| tx.send(m).is_ok();
    for p in plan {
        let Some(&(w_pt, h_pt)) = sizes.get(p.index) else {
            send(Msg::Error(format!("sheet {} has no size", p.index + 1)));
            return;
        };
        let (full_w, full_h) = (device_pixels(w_pt, p.scale), device_pixels(h_pt, p.scale));
        let whole = u64::from(full_w) * u64::from(full_h) * 4 <= WHOLE_SHEET_BYTES;
        if whole {
            match render_sheet(&mut renderer, p.index, w_pt, h_pt, p.scale) {
                Ok((w, h, rgba)) => {
                    if !send(Msg::Whole { w, h, rgba }) {
                        return;
                    }
                }
                Err(e) => {
                    send(Msg::Error(e));
                    return;
                }
            }
            continue;
        }
        let begin = render_whole(&mut renderer, p.index, w_pt, h_pt, PROBE_DPI / 72.0).map(|(probe_w, probe_h, probe)| Msg::Begin {
            w: full_w,
            h: full_h,
            probe_w,
            probe_h,
            probe,
        });
        match begin {
            Ok(m) => {
                if !send(m) {
                    return;
                }
            }
            Err(e) => {
                send(Msg::Error(e));
                return;
            }
        }
        for y in (0..full_h).step_by(BAND_ROWS as usize) {
            let rows = BAND_ROWS.min(full_h - y);
            match render_band(&mut renderer, p.index, p.scale, full_w, y, rows) {
                Ok(rgba) => {
                    if !send(Msg::Band { y, rows, rgba }) {
                        return;
                    }
                }
                Err(e) => {
                    send(Msg::Error(e));
                    return;
                }
            }
        }
        if !send(Msg::End) {
            return;
        }
    }
}

#[cfg(windows)]
mod gdi {
    //! The Win32 side. Handles are closed in `Drop` so an error anywhere leaves nothing open,
    //! and an unfinished document is aborted rather than printed half-way.

    use std::ffi::c_void;
    use std::sync::Arc;
    use std::sync::mpsc::{Receiver, sync_channel};

    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::Graphics::Gdi::{
        BITMAPINFO, CreateDCW, DEVMODE_COLLATE, DEVMODE_COLOR, DEVMODE_DUPLEX, DEVMODE_FIELD_FLAGS, DEVMODEW, DIB_RGB_COLORS, DeleteDC,
        GET_DEVICE_CAPS_INDEX, GetDeviceCaps, HDC, HORZRES, LOGPIXELSX, LOGPIXELSY, PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY, PHYSICALWIDTH,
        ResetDCW, SRCCOPY, StretchDIBits, VERTRES,
    };
    use windows::Win32::Graphics::Printing::{ClosePrinter, DocumentPropertiesW, GetDefaultPrinterW, OpenPrinterW, PRINTER_HANDLE};
    use windows::Win32::Storage::Xps::{
        AbortDoc, DC_COLLATE, DC_COPIES, DC_DUPLEX, DC_MAXEXTENT, DC_MINEXTENT, DC_ORIENTATION, DC_PAPERNAMES, DC_PAPERS, DC_PAPERSIZE, DOCINFOW,
        DeviceCapabilitiesW, EndDoc, EndPage, ExtEscape, PRINTER_DEVICE_CAPABILITIES, StartDocW, StartPage,
    };
    use windows::core::{PCWSTR, PWSTR};

    use super::{
        Dib, Duplex, Job, MAX_RENDER_DPI, MONO_MID_PERMILLE, Msg, PROBE_MONO_MID_PERMILLE, SheetPlan, classify, encode, over_white, render_worker,
        sheet_sizes,
    };

    // DEVMODE field flags and values (wingdi.h).
    const DM_ORIENTATION: u32 = 0x0000_0001;
    const DM_PAPERSIZE: u32 = 0x0000_0002;
    const DM_PAPERLENGTH: u32 = 0x0000_0004;
    const DM_PAPERWIDTH: u32 = 0x0000_0008;
    const DM_COPIES: u32 = 0x0000_0100;
    const DM_COLOR: u32 = 0x0000_0800;
    const DM_DUPLEX: u32 = 0x0000_1000;
    const DM_COLLATE: u32 = 0x0000_8000;
    const DMORIENT_PORTRAIT: i16 = 1;
    const DMORIENT_LANDSCAPE: i16 = 2;
    const DMPAPER_USER: i16 = 256;
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

        /// The custom paper sizes the driver takes, as (min width, min length, max width, max
        /// length) in tenths of a millimetre; `None` when it takes none.
        fn custom_extent(&self) -> Option<(i32, i32, i32, i32)> {
            let (min, max) = (self.capability(DC_MINEXTENT, None), self.capability(DC_MAXEXTENT, None));
            if min <= 0 || max <= 0 {
                return None;
            }
            let (min, max) = (min as u32, max as u32);
            Some(((min & 0xFFFF) as i32, (min >> 16) as i32, (max & 0xFFFF) as i32, (max >> 16) as i32))
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
        fn get(&self) -> &DEVMODEW {
            // SAFETY: the buffer is at least a DEVMODEW long and 8-byte aligned.
            unsafe { &*self.as_ptr() }
        }
        fn get_mut(&mut self) -> &mut DEVMODEW {
            // SAFETY: as `get`.
            unsafe { &mut *self.as_mut_ptr() }
        }
        fn set_orientation(&mut self, landscape: bool) {
            let dm = self.get_mut();
            dm.Anonymous1.Anonymous1.dmOrientation = if landscape { DMORIENT_LANDSCAPE } else { DMORIENT_PORTRAIT };
            dm.dmFields |= DEVMODE_FIELD_FLAGS(DM_ORIENTATION);
        }
        fn set_paper(&mut self, id: i16) {
            let dm = self.get_mut();
            dm.Anonymous1.Anonymous1.dmPaperSize = id;
            dm.dmFields |= DEVMODE_FIELD_FLAGS(DM_PAPERSIZE);
        }
        /// A custom size, width and length in tenths of a millimetre.
        fn set_custom_paper(&mut self, width: i32, length: i32) {
            let dm = self.get_mut();
            dm.Anonymous1.Anonymous1.dmPaperSize = DMPAPER_USER;
            dm.Anonymous1.Anonymous1.dmPaperWidth = width.clamp(0, i32::from(i16::MAX)) as i16;
            dm.Anonymous1.Anonymous1.dmPaperLength = length.clamp(0, i32::from(i16::MAX)) as i16;
            dm.dmFields |= DEVMODE_FIELD_FLAGS(DM_PAPERSIZE | DM_PAPERWIDTH | DM_PAPERLENGTH);
        }
        /// The custom size the driver kept, after validation.
        fn custom_paper(&self) -> (i32, i32) {
            let dm = self.get();
            // SAFETY: reading the paper union member, which every DEVMODE for a printer holds.
            unsafe { (i32::from(dm.Anonymous1.Anonymous1.dmPaperWidth), i32::from(dm.Anonymous1.Anonymous1.dmPaperLength)) }
        }
    }

    /// The paper in device pixels for one orientation: printable area, physical sheet, and the
    /// printable area's offset from the sheet's corner.
    #[derive(Clone, Copy, Debug)]
    struct Caps {
        res_w: i32,
        res_h: i32,
        phys_w: i32,
        phys_h: i32,
        off_x: i32,
        off_y: i32,
    }

    /// A printer device context with a document in progress.
    struct Document {
        hdc: HDC,
        started: bool,
        in_page: bool,
    }

    impl Document {
        fn open(printer: &Printer, dm: &DevMode) -> Result<Self, String> {
            let driver = wide("WINSPOOL");
            // SAFETY: the strings are NUL-terminated; the DEVMODE is a validated driver buffer.
            let hdc = unsafe { CreateDCW(PCWSTR(driver.as_ptr()), PCWSTR(printer.name.as_ptr()), PCWSTR::null(), Some(dm.as_ptr())) };
            if hdc.is_invalid() {
                return Err(last_error("opening the printer"));
            }
            Ok(Document { hdc, started: false, in_page: false })
        }

        fn cap(&self, index: GET_DEVICE_CAPS_INDEX) -> i32 {
            // SAFETY: hdc is a live device context.
            unsafe { GetDeviceCaps(Some(self.hdc), index) }
        }

        fn caps(&self) -> Caps {
            let (res_w, res_h) = (self.cap(HORZRES).max(1), self.cap(VERTRES).max(1));
            Caps {
                res_w,
                res_h,
                phys_w: self.cap(PHYSICALWIDTH).max(res_w),
                phys_h: self.cap(PHYSICALHEIGHT).max(res_h),
                off_x: self.cap(PHYSICALOFFSETX).max(0),
                off_y: self.cap(PHYSICALOFFSETY).max(0),
            }
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

        fn start_page(&mut self) -> Result<(), String> {
            // SAFETY: hdc has a document started.
            if unsafe { StartPage(self.hdc) } <= 0 {
                return Err(last_error("starting a page"));
            }
            self.in_page = true;
            Ok(())
        }

        /// Draw `dib` (`src` pixels) stretched onto `dest` = (x, y, width, height).
        fn draw(&mut self, dib: &Dib, dest: (i32, i32, i32, i32), src: (u32, u32)) -> Result<(), String> {
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
            if lines <= 0 { Err(last_error("drawing the page")) } else { Ok(()) }
        }

        fn end_page(&mut self) -> Result<(), String> {
            self.in_page = false;
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
                    if self.in_page {
                        EndPage(self.hdc);
                    }
                    AbortDoc(self.hdc);
                }
                let _ = DeleteDC(self.hdc);
            }
        }
    }

    /// Diagnostics: with `DOCUVIEW_PRINT_DUMP=<folder>` set, every image sent to the driver is
    /// also written there (`<label>.<depth>.png`, or `.jpg` for a JPEG; a streamed sheet's bands
    /// are `sheet-N-yROW`), so the exact output can be looked at without a printer.
    fn dump(dib: &Dib, rgba: &[u8], width: u32, height: u32, label: &str) {
        let Some(dir) = std::env::var_os("DOCUVIEW_PRINT_DUMP") else { return };
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let written = match dib.depth {
            "jpeg" => std::fs::write(dir.join(format!("{label}.jpeg.jpg")), &dib.bits),
            depth => {
                // Unpack the DIB rows (bottom-up) back into a top-down grey image of what was sent.
                let (w, h) = (width as usize, height as usize);
                let mut grey = vec![0u8; w * h];
                match depth {
                    "mono" => {
                        let row_bytes = w.div_ceil(32) * 4;
                        for y in 0..h {
                            let row = dib.bits.get((h - 1 - y) * row_bytes..).unwrap_or(&[]);
                            for x in 0..w {
                                grey[y * w + x] = if row.get(x / 8).is_some_and(|b| b & (0x80 >> (x % 8)) != 0) { 255 } else { 0 };
                            }
                        }
                    }
                    "gray" => {
                        let row_bytes = (w + 3) & !3;
                        for y in 0..h {
                            if let Some(src) = dib.bits.get((h - 1 - y) * row_bytes..(h - 1 - y) * row_bytes + w) {
                                grey[y * w..(y + 1) * w].copy_from_slice(src);
                            }
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
                    Ok(()) => std::fs::write(dir.join(format!("{label}.{depth}.png")), &out),
                    Err(e) => Err(std::io::Error::other(e.to_string())),
                }
            }
        };
        if let Err(e) = written {
            eprintln!("DOCUVIEW_PRINT_DUMP: {label} not written: {e}");
        }
    }

    /// Points to tenths of a millimetre.
    fn tenth_mm(pt: f32) -> i32 {
        (f64::from(pt) * 25.4 / 72.0 * 10.0).round() as i32
    }

    /// Where a sheet of `w_pt` × `h_pt` goes on paper described by `caps`, and the scale to
    /// render it at.
    fn place(index: usize, landscape: bool, w_pt: f32, h_pt: f32, caps: Caps, dpi: (i32, i32), shrink_to_fit: bool) -> SheetPlan {
        let sheet_w = (f64::from(w_pt) * f64::from(dpi.0) / 72.0).round() as i32;
        let sheet_h = (f64::from(h_pt) * f64::from(dpi.1) / 72.0).round() as i32;
        let (dest_w, dest_h, x, y) = if shrink_to_fit {
            let scale = if sheet_w > caps.res_w || sheet_h > caps.res_h {
                (f64::from(caps.res_w) / f64::from(sheet_w.max(1))).min(f64::from(caps.res_h) / f64::from(sheet_h.max(1)))
            } else {
                1.0
            };
            let (dw, dh) = ((f64::from(sheet_w) * scale).round() as i32, (f64::from(sheet_h) * scale).round() as i32);
            // Centred on the physical sheet, in printable-area coordinates.
            (dw.max(1), dh.max(1), (caps.phys_w - dw) / 2 - caps.off_x, (caps.phys_h - dh) / 2 - caps.off_y)
        } else {
            (sheet_w.max(1), sheet_h.max(1), -caps.off_x, -caps.off_y)
        };
        // One device pixel per printer dot, up to MAX_RENDER_DPI; the driver scales the rest.
        let scale = ((f64::from(dest_w) / f64::from(w_pt.max(1.0))) as f32).min(MAX_RENDER_DPI / 72.0);
        SheetPlan { index, landscape, x, y, dest_w, dest_h, scale }
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

        // Paper: the driver's own size for the first sheet (portrait dimensions), within a
        // millimetre; else a custom size when the driver takes one of that size.
        let (short, long) = (tenth_mm(first.0.min(first.1)), tenth_mm(first.0.max(first.1)));
        let papers = printer.papers();
        let paper = papers.iter().position(|&(_, w, h)| (w - short).abs() <= 10 && (h - long).abs() <= 10);
        let mut custom = false;
        match (paper, job.shrink_to_fit) {
            (Some(i), _) => {
                if let Some(&(id, _, _)) = papers.get(i) {
                    dm.set_paper(id);
                    let _ = printer.paper_name(i);
                }
            }
            (None, true) => {} // the driver's default paper; sheets are fitted to it below
            (None, false) => {
                let fits = printer
                    .custom_extent()
                    .is_some_and(|(min_w, min_l, max_w, max_l)| (min_w..=max_w).contains(&short) && (min_l..=max_l).contains(&long));
                if !fits {
                    return Err(format!(
                        "{name} has no {:.2} × {:.2} inch paper; choose a paper size it has, or add this one in its driver",
                        first.0 / 72.0,
                        first.1 / 72.0
                    ));
                }
                dm.set_custom_paper(short, long);
                custom = true;
            }
        }
        let first_landscape = first.0 > first.1 + 1.0;
        let turns = printer.capability(DC_ORIENTATION, None) != 0;
        if turns {
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
        if custom {
            let (w, l) = dm.custom_paper();
            if (w - short).abs() > 10 || (l - long).abs() > 10 {
                return Err(format!(
                    "{name} would not take a custom {:.2} × {:.2} inch paper; choose a paper size it has, or add this one in its driver",
                    first.0 / 72.0,
                    first.1 / 72.0
                ));
            }
        }

        let mut doc = Document::open(&printer, &dm)?;
        let dpi = (doc.cap(LOGPIXELSX).max(1), doc.cap(LOGPIXELSY).max(1));
        let postscript = doc.supports_escape(POSTSCRIPT_IDENTIFY);
        let jpeg = postscript && doc.supports_escape(CHECKJPEGFORMAT);

        // The paper's geometry in each orientation the job uses (ResetDC before the document
        // starts costs nothing), so every sheet can be planned up front for the render thread.
        let mut caps_first = doc.caps();
        let mut caps_other = caps_first;
        let other_used = sizes.iter().any(|&(w, h)| (w > h + 1.0) != first_landscape);
        if turns && other_used {
            dm.set_orientation(!first_landscape);
            printer.validate(&mut dm)?;
            doc.reset(&dm)?;
            caps_other = doc.caps();
            dm.set_orientation(first_landscape);
            printer.validate(&mut dm)?;
            doc.reset(&dm)?;
            caps_first = doc.caps();
        }
        let order: Vec<usize> = if job.collate || our_copies == 1 {
            (0..our_copies as usize).flat_map(|_| 0..count).collect()
        } else {
            (0..count).flat_map(|i| std::iter::repeat_n(i, our_copies as usize)).collect()
        };
        let plan: Vec<SheetPlan> = order
            .iter()
            .map(|&i| {
                let (w_pt, h_pt) = sizes.get(i).copied().unwrap_or(first);
                let landscape = w_pt > h_pt + 1.0;
                let caps = if landscape == first_landscape || !turns { caps_first } else { caps_other };
                place(i, landscape && turns || (!turns && first_landscape), w_pt, h_pt, caps, dpi, job.shrink_to_fit)
            })
            .collect();

        doc.start(job.title)?;
        progress(0, count);

        // The render thread works one sheet ahead; the channel holds a couple of bands or sheets.
        let (tx, rx) = sync_channel::<Msg>(2);
        let worker_bytes = Arc::clone(&bytes);
        let worker_sizes = sizes.clone();
        let worker_plan = plan.clone();
        let result = std::thread::scope(|s| {
            s.spawn(move || render_worker(worker_bytes, worker_sizes, worker_plan, tx));
            send_all(&mut doc, &printer, &mut dm, &plan, &rx, job, jpeg, dpi, first_landscape, turns, progress, count)
        });
        result?;
        doc.finish()?;
        Ok(format!("spooled {count} sheet(s)"))
    }

    /// The sending side: takes rendered sheets and bands off the channel in `plan` order and
    /// draws them.
    #[allow(clippy::too_many_arguments)]
    fn send_all(
        doc: &mut Document,
        printer: &Printer,
        dm: &mut DevMode,
        plan: &[SheetPlan],
        rx: &Receiver<Msg>,
        job: &Job,
        jpeg: bool,
        dpi: (i32, i32),
        first_landscape: bool,
        turns: bool,
        progress: &mut dyn FnMut(usize, usize),
        count: usize,
    ) -> Result<(), String> {
        let mut landscape = first_landscape;
        let dpi_f = f64::from(dpi.0);
        let recv = || rx.recv().map_err(|_| "the renderer stopped without a result".to_string());
        for (seq, p) in plan.iter().enumerate() {
            let n = p.index + 1;
            if turns && p.landscape != landscape {
                dm.set_orientation(p.landscape);
                printer.validate(dm)?;
                doc.reset(dm)?;
                landscape = p.landscape;
            }
            let dest = (p.x, p.y, p.dest_w, p.dest_h);
            match recv()? {
                Msg::Whole { w, h, mut rgba } => {
                    over_white(&mut rgba, job.grayscale);
                    let depth = classify(&rgba, job.grayscale, MONO_MID_PERMILLE);
                    let dib = encode(&rgba, w, h, depth, jpeg, dpi_f, 0)?;
                    dump(&dib, &rgba, w, h, &format!("sheet-{n}"));
                    drop(rgba);
                    doc.start_page()?;
                    doc.draw(&dib, dest, (w, h))?;
                    doc.end_page()?;
                }
                Msg::Begin { w, h, probe_w, probe_h, mut probe } => {
                    over_white(&mut probe, job.grayscale);
                    let depth = classify(&probe, job.grayscale, PROBE_MONO_MID_PERMILLE);
                    drop(probe);
                    let _ = (probe_w, probe_h);
                    doc.start_page()?;
                    loop {
                        match recv()? {
                            Msg::Band { y, rows, mut rgba } => {
                                over_white(&mut rgba, job.grayscale);
                                let dib = encode(&rgba, w, rows, depth, jpeg, dpi_f, y as usize)?;
                                dump(&dib, &rgba, w, rows, &format!("sheet-{n}-y{y}"));
                                drop(rgba);
                                // The band's rows mapped onto the sheet's destination rectangle,
                                // rounded so consecutive bands meet exactly.
                                let row_to_dest = |r: u32| p.y + (i64::from(r) * i64::from(p.dest_h) / i64::from(h.max(1))) as i32;
                                let (y0, y1) = (row_to_dest(y), row_to_dest(y + rows));
                                doc.draw(&dib, (p.x, y0, p.dest_w, (y1 - y0).max(1)), (w, rows))?;
                            }
                            Msg::End => break,
                            Msg::Error(e) => return Err(e),
                            Msg::Whole { .. } | Msg::Begin { .. } => {
                                return Err(format!("sheet {n}: the renderer sent a sheet while a band was expected"));
                            }
                        }
                    }
                    doc.end_page()?;
                }
                Msg::Error(e) => return Err(e),
                Msg::Band { .. } | Msg::End => return Err(format!("sheet {n}: the renderer sent a band while a sheet was expected")),
            }
            progress((seq + 1).min(count), count);
        }
        Ok(())
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
        assert_eq!(classify(&page(&text), false, MONO_MID_PERMILLE), Depth::Mono);
        let mut shaded = text.clone();
        shaded[100..170].fill([230, 230, 230]); // a light grey box on 7% of the page: still dithered mono
        assert_eq!(classify(&page(&shaded), false, MONO_MID_PERMILLE), Depth::Mono);
        let mut photo = text.clone();
        photo[100..400].fill([128, 128, 128]); // a grey picture on 30% of the page
        assert_eq!(classify(&page(&photo), false, MONO_MID_PERMILLE), Depth::Gray);
        let mut colour = text.clone();
        colour[0] = [31, 95, 191];
        assert_eq!(classify(&page(&colour), false, MONO_MID_PERMILLE), Depth::Color);
        assert_eq!(classify(&page(&colour), true, MONO_MID_PERMILLE), Depth::Mono, "a greyscale job never sends colour");
    }

    #[test]
    fn dibs_have_padded_bottom_up_rows() {
        let mut rgba = page(&[[0, 0, 0], [255, 255, 255], [255, 255, 255]]);
        rgba.extend(page(&[[255, 255, 255], [255, 255, 255], [0, 0, 0]])); // 3 x 2
        let mono = dib_mono(&rgba, 3, 2, 0);
        assert_eq!(mono.bits.len(), 8, "two rows of four bytes");
        assert_eq!(mono.bits[0], 0b1100_0000, "bottom row first: white, white, black");
        assert_eq!(mono.bits[4], 0b0110_0000, "top row: black, white, white");
        // Pure white and pure black stay solid; a flat 50% grey dithers to half dots.
        let dots = |level: u8| -> u32 { dib_mono(&page(&[[level; 3]; 64]), 8, 8, 0).bits.iter().map(|b| b.count_ones()).sum() };
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

    #[test]
    fn bands_continue_the_dither_pattern() {
        // A 50% grey sheet of 16 rows dithered whole equals the same sheet dithered as two
        // 8-row bands with their y offsets: no seam.
        let grey = page(&[[128; 3]; 8 * 16]);
        let whole = dib_mono(&grey, 8, 16, 0);
        let top = dib_mono(&grey[..8 * 8 * 4], 8, 8, 0);
        let bottom = dib_mono(&grey[8 * 8 * 4..], 8, 8, 8);
        // Bottom-up rows: the whole image's first 8 rows are the bottom band's.
        assert_eq!(&whole.bits[..32], &bottom.bits[..]);
        assert_eq!(&whole.bits[32..], &top.bits[..]);
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
        // The same sheet in BAND_ROWS bands equals the whole render, row for row.
        for y in (0..h).step_by(BAND_ROWS as usize) {
            let rows = BAND_ROWS.min(h - y);
            let band = render_band(&mut renderer, 0, scale, w, y, rows).unwrap();
            assert_eq!(band.len(), (w * rows * 4) as usize);
            assert_eq!(&band[..], &rgba[(y * w * 4) as usize..((y + rows) * w * 4) as usize], "band at row {y}");
        }
        // A sheet within the caps renders whole, at the asked scale.
        let small = Arc::new(pdf_with_rect(612.0, 792.0, [0.0, 0.0, 10.0, 10.0]));
        let mut renderer = PageRenderer::new(small, pdfcraft_render::RenderConfig::default());
        let (w, h, _) = render_sheet(&mut renderer, 0, 612.0, 792.0, scale).unwrap();
        assert_eq!((w, h), (5100, 6600));
    }

    #[test]
    fn the_render_worker_streams_large_sheets_and_sends_small_ones_whole() {
        let pdf = Arc::new(pdf_with_rect(792.0, 1224.0, [100.0, 100.0, 50.0, 50.0]));
        let scale = 600.0 / 72.0;
        let plan = vec![
            SheetPlan { index: 0, landscape: false, x: 0, y: 0, dest_w: 6600, dest_h: 10200, scale },
            SheetPlan { index: 0, landscape: false, x: 0, y: 0, dest_w: 1100, dest_h: 1700, scale: 100.0 / 72.0 },
        ];
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        let sizes = vec![(792.0, 1224.0)];
        let worker = std::thread::spawn(move || render_worker(pdf, sizes, plan, tx));
        let mut bands = 0;
        let mut rows = 0;
        match rx.recv().unwrap() {
            Msg::Begin { w, h, probe_w, probe_h, .. } => {
                assert_eq!((w, h), (6600, 10200));
                assert!(probe_w < 4096 && probe_h < 4096, "the probe is small: {probe_w} x {probe_h}");
            }
            _ => panic!("a Tabloid sheet at 600 dpi streams"),
        }
        loop {
            match rx.recv().unwrap() {
                Msg::Band { rows: r, rgba, .. } => {
                    bands += 1;
                    rows += r;
                    assert_eq!(rgba.len(), (6600 * r * 4) as usize);
                }
                Msg::End => break,
                Msg::Error(e) => panic!("{e}"),
                _ => panic!("unexpected message"),
            }
        }
        assert_eq!(rows, 10200);
        assert_eq!(bands, 10200_u32.div_ceil(BAND_ROWS));
        match rx.recv().unwrap() {
            Msg::Whole { w, h, .. } => assert_eq!((w, h), (1100, 1700)),
            _ => panic!("a small sheet comes whole"),
        }
        assert!(rx.recv().is_err(), "nothing after the last sheet");
        worker.join().unwrap();
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
