// src/render/font.rs — FreeType glyph atlas, kitty-parity rendering.
//
// Pipeline:
//   FT_LOAD_TARGET_LCD + FT_LOAD_FORCE_AUTOHINT + FT_LCD_FILTER_LIGHT
//   → RGB bitmap (3 bytes per screen pixel) → GL_RGB atlas texture
//   → LCD shader does per-channel coverage blend
//
// Falls back to FT_LOAD_TARGET_NORMAL grayscale if LCD is unavailable.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_long, c_short, c_uchar, c_uint, c_ulong, c_ushort, c_void};

// ── FreeType ABI (2.x, 64-bit Linux) ─────────────────────────────────────────

type FtError = c_int;
type FtUInt = c_uint;
type FtInt = c_int;
type FtLong = c_long;
type FtULong = c_ulong;
type FtPos = c_long;
type FtFixed = c_long;

#[repr(C)]
struct FtVector {
    x: FtPos,
    y: FtPos,
}
#[repr(C)]
struct FtBbox {
    xmin: FtPos,
    ymin: FtPos,
    xmax: FtPos,
    ymax: FtPos,
}
#[repr(C)]
struct FtGeneric {
    data: *mut c_void,
    fin: *mut c_void,
}

#[repr(C)]
struct FtBitmap {
    rows: c_uint,
    width: c_uint,
    pitch: c_int,
    buffer: *mut c_uchar,
    num_grays: c_ushort,
    pixel_mode: c_uchar,
    palette_mode: c_uchar,
    palette: *mut c_void,
}

#[repr(C)]
struct FtGlyphMetrics {
    width: FtPos,
    height: FtPos,
    hbx: FtPos,
    hby: FtPos,
    ha: FtPos,
    vbx: FtPos,
    vby: FtPos,
    va: FtPos,
}

// FT_GlyphSlotRec — only the fields we need, rest padded.
#[repr(C)]
struct FtGlyphSlot {
    library: *mut c_void,
    face: *mut c_void,
    next: *mut c_void,
    idx: c_uint,
    generic: FtGeneric,
    metrics: FtGlyphMetrics,
    lha: FtFixed,
    lva: FtFixed,
    advance: FtVector,
    format: c_uint,
    bitmap: FtBitmap,
    bitmap_left: c_int,
    bitmap_top: c_int,
    _pad: [u8; 272],
}

// FT_FaceRec — just enough to reach face->glyph.
#[repr(C)]
struct FtFaceRec {
    num_faces: FtLong,
    face_index: FtLong,
    face_flags: FtLong,
    style_flags: FtLong,
    num_glyphs: FtLong,
    family_name: *const c_char,
    style_name: *const c_char,
    num_fixed_sizes: c_int,
    available_sizes: *mut c_void,
    num_charmaps: c_int,
    charmaps: *mut c_void,
    generic: FtGeneric,
    bbox: FtBbox,
    units_per_em: c_ushort,
    ascender: c_short,
    descender: c_short,
    height: c_short,
    max_advance_width: c_short,
    max_advance_height: c_short,
    underline_position: c_short,
    underline_thickness: c_short,
    glyph: *mut FtGlyphSlot,
    _pad: [u8; 128],
}

const FT_PIXEL_MODE_GRAY: u8 = 2;
const FT_PIXEL_MODE_LCD: u8 = 5;

// Load flags
const FT_LOAD_RENDER: c_int = 1 << 2;
const FT_LOAD_FORCE_AUTOHINT: c_int = 1 << 5;
const FT_LOAD_TARGET_NORMAL: c_int = 0;
const FT_LOAD_TARGET_LCD: c_int = 7 << 16; // FT_LOAD_TARGET_(FT_RENDER_MODE_LCD)

const FT_LCD_FILTER_LIGHT: c_int = 2;

type FtF26Dot6 = c_long;

#[link(name = "freetype")]
extern "C" {
    fn FT_Init_FreeType(alib: *mut *mut c_void) -> FtError;
    fn FT_Done_FreeType(lib: *mut c_void) -> FtError;
    fn FT_New_Face(
        lib: *mut c_void,
        filepathname: *const c_char,
        face_index: FtLong,
        aface: *mut *mut c_void,
    ) -> FtError;
    fn FT_Done_Face(face: *mut c_void) -> FtError;
    fn FT_Set_Pixel_Sizes(face: *mut c_void, pixel_width: FtUInt, pixel_height: FtUInt) -> FtError;
    /// More reliable than FT_Set_Pixel_Sizes for CFF/OTF fonts.
    /// char_width and char_height are in 26.6 fixed-point (points × 64).
    /// horz_resolution / vert_resolution are in dpi (0 = 72dpi default).
    fn FT_Set_Char_Size(
        face: *mut c_void,
        char_width: FtF26Dot6,
        char_height: FtF26Dot6,
        horz_resolution: FtUInt,
        vert_resolution: FtUInt,
    ) -> FtError;
    fn FT_Load_Char(face: *mut c_void, char_code: FtULong, load_flags: FtInt) -> FtError;
    fn FT_Library_SetLcdFilter(library: *mut c_void, filter: FtInt) -> FtError;
}

// ── GlyphInfo ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct GlyphInfo {
    pub tex_id: u32,
    pub uv: [f32; 4], // [u0, v0, u1, v1]
    pub px_w: i32,    // screen pixels wide
    pub px_h: i32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance: i32, // in screen pixels
    pub lcd: bool,
}

// ── Atlas ─────────────────────────────────────────────────────────────────────

struct AtlasRow {
    tex_id: u32,
    cursor: u32, // next free x in pixels
    height: u32, // texture height in pixels
    lcd: bool,
}

// Wide enough for a full ASCII + common unicode set at one size.
const ATLAS_W: u32 = 2048;

// ── FontAtlas ─────────────────────────────────────────────────────────────────

pub struct FontAtlas {
    lib: *mut c_void,
    face: *mut c_void,
    cache: HashMap<(u32, char), GlyphInfo>,
    rows: HashMap<(u32, bool), AtlasRow>, // key: (size_px, is_lcd)
    pub lcd: bool,
}

unsafe impl Send for FontAtlas {}

impl FontAtlas {
    pub fn new(path: &str) -> Result<Self> {
        unsafe {
            let mut lib: *mut c_void = std::ptr::null_mut();
            let e = FT_Init_FreeType(&mut lib);
            anyhow::ensure!(e == 0, "FT_Init_FreeType: {e}");

            let cpath = CString::new(path).context("nul in font path")?;
            let mut face: *mut c_void = std::ptr::null_mut();
            let e = FT_New_Face(lib, cpath.as_ptr(), 0, &mut face);
            if e != 0 {
                FT_Done_FreeType(lib);
                anyhow::bail!("FT_New_Face({path}): {e}");
            }

            // FT_LCD_FILTER_LIGHT = 2. Returns non-zero if LCD rendering is
            // not compiled into this FreeType build.
            let lcd_err = FT_Library_SetLcdFilter(lib, FT_LCD_FILTER_LIGHT);
            let lcd = lcd_err == 0;
            tracing::info!(
                "FontAtlas: {path} | subpixel LCD: {} (FT_SetLcdFilter err={lcd_err})",
                if lcd {
                    "YES"
                } else {
                    "NO — grayscale fallback"
                }
            );

            Ok(Self {
                lib,
                face,
                cache: HashMap::new(),
                rows: HashMap::new(),
                lcd,
            })
        }
    }

    /// Look up a glyph, rasterising on first access.
    pub fn glyph(&mut self, ch: char, size_px: u32) -> Option<GlyphInfo> {
        if let Some(&g) = self.cache.get(&(size_px, ch)) {
            return Some(g);
        }
        self.rasterise(ch, size_px)
    }

    fn rasterise(&mut self, ch: char, size_px: u32) -> Option<GlyphInfo> {
        unsafe {
            // Use FT_Set_Char_Size instead of FT_Set_Pixel_Sizes.
            // FT_Set_Pixel_Sizes(face, 0, N) is equivalent to
            // FT_Set_Char_Size(face, 0, N*64, 0, 0) but some CFF fonts
            // (including Nerd Font variants) respond better to the latter.
            let e = FT_Set_Char_Size(self.face, 0, (size_px * 64) as FtF26Dot6, 96, 96);
            if e != 0 {
                tracing::warn!("FT_Set_Char_Size({size_px}): {e}");
                return None;
            }

            // Try LCD first; if it fails (err 19 = FT_Err_Invalid_Pixel_Size
            // or err 9 = FT_Err_Invalid_Argument) fall back to normal gray.
            let lcd_flags = FT_LOAD_RENDER | FT_LOAD_TARGET_LCD | FT_LOAD_FORCE_AUTOHINT;
            let gray_flags = FT_LOAD_RENDER | FT_LOAD_TARGET_NORMAL | FT_LOAD_FORCE_AUTOHINT;

            let mut load_err = if self.lcd {
                FT_Load_Char(self.face, ch as FtULong, lcd_flags)
            } else {
                1 // force gray path
            };

            let used_lcd;
            if !self.lcd || load_err != 0 {
                load_err = FT_Load_Char(self.face, ch as FtULong, gray_flags);
                used_lcd = false;
                if load_err != 0 {
                    tracing::warn!(
                        "FT_Load_Char('{}' U+{:04X}, sz={size_px}) gray err={load_err}",
                        ch,
                        ch as u32,
                    );
                    return None;
                }
            } else {
                used_lcd = true;
            }

            // 3. Read bitmap from the glyph slot.
            let face_rec = &*(self.face as *const FtFaceRec);
            let slot = &*face_rec.glyph;
            let bmp = &slot.bitmap;

            // Confirm actual pixel mode — LCD load might still produce gray
            // if the hinter overrode it.
            let is_lcd = used_lcd && bmp.pixel_mode == FT_PIXEL_MODE_LCD;
            let bmp_w = bmp.width; // raw buffer width (3× for LCD)
            let bmp_h = bmp.rows;
            let pitch = bmp.pitch.unsigned_abs() as usize;
            let buf = bmp.buffer;
            let bl = slot.bitmap_left;
            let bt = slot.bitmap_top;
            // advance.x is 26.6 fixed-point
            let adv = (slot.advance.x >> 6) as i32;

            // Screen-pixel width
            let scr_w = if is_lcd { bmp_w / 3 } else { bmp_w };

            // Log first glyph per atlas row so we can verify dimensions.
            tracing::debug!(
                "glyph '{}' U+{:04X} sz={size_px} mode={} bmp={}x{} scr_w={scr_w} \
                 adv={adv} bl={bl} bt={bt}",
                ch,
                ch as u32,
                bmp.pixel_mode,
                bmp_w,
                bmp_h
            );

            // 4. Whitespace / zero-size glyphs: cache advance, no texture.
            if bmp_w == 0 || bmp_h == 0 || buf.is_null() {
                let info = GlyphInfo {
                    tex_id: 0,
                    uv: [0.0; 4],
                    px_w: 0,
                    px_h: 0,
                    bearing_x: bl,
                    bearing_y: bt,
                    advance: adv,
                    lcd: is_lcd,
                };
                self.cache.insert((size_px, ch), info);
                return Some(info);
            }

            // 5. Get or create atlas row.
            let is_lcd_row = is_lcd; // borrow-checker alias
            let row = self.rows.entry((size_px, is_lcd_row)).or_insert_with(|| {
                let h = size_px + 4;
                let (ifmt, fmt) = if is_lcd_row {
                    (gl::RGB8 as i32, gl::RGB)
                } else {
                    (gl::R8 as i32, gl::RED)
                };
                let mut tid = 0u32;
                gl::GenTextures(1, &mut tid);
                gl::BindTexture(gl::TEXTURE_2D, tid);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
                // Allocate full row at once.
                gl::TexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    ifmt,
                    ATLAS_W as i32,
                    h as i32,
                    0,
                    fmt,
                    gl::UNSIGNED_BYTE,
                    std::ptr::null(),
                );
                gl::BindTexture(gl::TEXTURE_2D, 0);
                tracing::debug!(
                    "new atlas row: size={size_px} lcd={is_lcd_row} tex={tid} {}x{h}",
                    ATLAS_W
                );
                AtlasRow {
                    tex_id: tid,
                    cursor: 0,
                    height: h,
                    lcd: is_lcd_row,
                }
            });

            if row.cursor + scr_w > ATLAS_W {
                tracing::warn!("atlas full for size={size_px} lcd={is_lcd_row}");
                return None;
            }

            // 6. Upload glyph pixels — pack rows if pitch ≠ row byte count.
            let row_bytes = if is_lcd {
                scr_w as usize * 3
            } else {
                scr_w as usize
            };
            let upload_fmt = if is_lcd { gl::RGB } else { gl::RED };

            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            gl::BindTexture(gl::TEXTURE_2D, row.tex_id);

            if pitch == row_bytes {
                gl::TexSubImage2D(
                    gl::TEXTURE_2D,
                    0,
                    row.cursor as i32,
                    0,
                    scr_w as i32,
                    bmp_h as i32,
                    upload_fmt,
                    gl::UNSIGNED_BYTE,
                    buf as *const _,
                );
            } else {
                // Rows have padding — copy each row into a packed buffer.
                let total = row_bytes * bmp_h as usize;
                let mut packed = Vec::with_capacity(total);
                for r in 0..bmp_h as usize {
                    let src = std::slice::from_raw_parts(buf.add(r * pitch), row_bytes);
                    packed.extend_from_slice(src);
                }
                gl::TexSubImage2D(
                    gl::TEXTURE_2D,
                    0,
                    row.cursor as i32,
                    0,
                    scr_w as i32,
                    bmp_h as i32,
                    upload_fmt,
                    gl::UNSIGNED_BYTE,
                    packed.as_ptr() as *const _,
                );
            }

            gl::BindTexture(gl::TEXTURE_2D, 0);
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 4); // restore default

            // 7. Compute UVs.
            let u0 = row.cursor as f32 / ATLAS_W as f32;
            let u1 = (row.cursor + scr_w) as f32 / ATLAS_W as f32;
            let v0 = 0.0_f32;
            let v1 = bmp_h as f32 / row.height as f32;

            let info = GlyphInfo {
                tex_id: row.tex_id,
                uv: [u0, v0, u1, v1],
                px_w: scr_w as i32,
                px_h: bmp_h as i32,
                bearing_x: bl,
                bearing_y: bt,
                advance: adv,
                lcd: is_lcd,
            };

            row.cursor += scr_w + 1;
            self.cache.insert((size_px, ch), info);
            Some(info)
        }
    }

    pub fn measure(&mut self, text: &str, size_px: u32) -> i32 {
        text.chars()
            .map(|ch| {
                self.glyph(ch, size_px)
                    .map(|g| g.advance)
                    .unwrap_or((size_px / 2) as i32)
            })
            .sum()
    }
}

impl Drop for FontAtlas {
    fn drop(&mut self) {
        unsafe {
            for row in self.rows.values() {
                let t = row.tex_id;
                gl::DeleteTextures(1, &t);
            }
            FT_Done_Face(self.face);
            FT_Done_FreeType(self.lib);
        }
    }
}
