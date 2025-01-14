// sixela::output
//
// TOC
// - struct SixelNode
// - struct SixelOutput

use crate::{
    dither::DitherConf, pixelformat::sixel_helper_normalize_pixelformat, SixelError, SixelResult,
};
use alloc::{format, vec};
use devela::{sys::Write as IoWrite, String, Vec};

mod dither_fns;
use dither_fns::*;

mod builder;
mod enums;
pub use {builder::*, enums::*};

pub(crate) const SIXEL_PALETTE_MAX: usize = 256;
// const SIXEL_USE_DEPRECATED_SYMBOLS: usize = 1;
// const SIXEL_ALLOCATE_BYTES_MAX: usize = 10_248 * 1_024 * 128; /* up to 128M */
// const SIXEL_WIDTH_LIMIT: usize = 1_000_000;
// const SIXEL_HEIGHT_LIMIT: usize = 1_000_000;

// loader settings
// const SIXEL_DEFAULT_GIF_DELAY: usize = 1;

const DCS_START_7BIT: &str = "\x1BP";
const DCS_START_8BIT: &str = "\u{220}";
const DCS_END_7BIT: &str = "\x1B\\";
const DCS_END_8BIT: &str = "\u{234}";
const SCREEN_PACKET_SIZE: usize = 256;
const PALETTE_HIT: i32 = 1;
const PALETTE_CHANGE: i32 = 2;
// enum Palette { // TODO:MAYBE
//     HIT,
//     CHANGE,
// }

/// Represents a single sixel tile with color and spatial properties.
///
/// Holds the palette index, x-coordinates, and a map of color data
/// for efficient rendering of individual sixel tiles.
///
/// # Adaptation
/// - Derived from `sixel_node` struct in the `libsixel` C library.
#[derive(Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct SixelNode {
    /// Index of the color in the palette.
    pub pal: i32,
    /// Start x-coordinate of the tile.
    pub sx: i32,
    /// End x-coordinate of the tile.
    pub mx: i32,
    /// Color data map for the tile.
    pub map: Vec<u8>,
}

/// Handles sixel data output to a specified writer destination.
///
/// Abstracts over writing sixel-encoded data,
/// supporting various output targets such as files or terminal streams.
///
/// # Adaptation
/// - Derived from `sixel_output` struct in the `libsixel` C library.
#[derive(Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct SixelOutput<W: IoWrite> {
    /* public fields
     */
    /// Palette selection mode.
    pub palette_type: PaletteType,

    /// Writer for output, managing data destination.
    pub fn_write: W,

    /// Last saved pixel value.
    pub save_pixel: u8,
    /// Count of consecutive saved pixels.
    pub save_count: i32,
    /// Currently active palette index.
    pub active_palette: i32,

    /// Collection of sixel nodes for dithering.
    pub nodes: Vec<SixelNode>,

    /// Flag to allow penetration of the multiplexer.
    pub penetrate_multiplexer: bool,
    /// Policy for encoding decisions.
    pub encode_policy: EncodePolicy,

    /// Buffer for output data.
    pub buffer: String,

    /* private compatibility flags
     */
    /// Indicates 8-bit terminal support.
    ///
    /// `false` for 7-bit, `true` for 8-bit.
    pub(crate) has_8bit_control: bool,

    /// Sixel scrolling support flag.
    ///
    /// `false` if terminal supports scrolling, `true` if not.
    pub(crate) has_sixel_scrolling: bool,

    /// Argument limit for repeat introducer (DECGRI).
    ///
    /// `false` if limited to 255, `true` if unlimited.
    pub(crate) has_gri_arg_limit: bool,

    /// DECSDM (CSI ? 80 h) sixel scrolling glitch flag.
    ///
    /// `false` enables sixel scrolling, `true` disables it.
    pub(crate) has_sdm_glitch: bool,

    /// Flag to skip DCS envelope handling.
    ///
    /// `false` to process, `true` to skip.
    pub(crate) skip_dcs_envelope: bool,
}

#[allow(dead_code, reason = "crate private struct")]
impl<W: IoWrite> SixelOutput<W> {
    /// Packet size limit.
    pub(crate) const PACKET_SIZE: usize = 16_384;

    /// Create new output context object
    #[inline]
    pub fn new(fn_write: W) -> Self {
        Self {
            has_8bit_control: false,
            has_sdm_glitch: false,
            has_gri_arg_limit: true,
            skip_dcs_envelope: false,
            palette_type: PaletteType::Auto,
            fn_write,
            save_pixel: 0,
            save_count: 0,
            active_palette: -1,
            nodes: Vec::new(),
            penetrate_multiplexer: false,
            encode_policy: EncodePolicy::Auto,
            has_sixel_scrolling: false,
            buffer: String::new(),
        }
    }

    /// Get 8bit output mode which indicates whether it uses C1 control characters.
    #[inline]
    #[must_use]
    pub fn get_8bit_availability(&self) -> bool {
        self.has_8bit_control
    }
    /// Set 8bit output mode state.
    #[inline]
    pub fn set_8bit_availability(&mut self, availability: bool) {
        self.has_8bit_control = availability;
    }

    /// Set limit for repeat introducer (DECGRI).
    ///
    /// `false` if limited to 255, `true` if unlimited.
    #[inline]
    pub fn set_gri_arg_limit(&mut self, value: bool) {
        self.has_gri_arg_limit = value;
    }

    /// Set GNU Screen penetration.
    #[inline]
    pub fn set_penetrate_multiplexer(&mut self, penetrate: bool) {
        self.penetrate_multiplexer = penetrate;
    }

    /// Set whether we skip DCS envelope.
    #[inline]
    pub fn set_skip_dcs_envelope(&mut self, skip: bool) {
        self.skip_dcs_envelope = skip;
    }

    /// Set the palette type.
    #[inline]
    pub fn set_palette_type(&mut self, palettetype: PaletteType) {
        self.palette_type = palettetype;
    }

    /// Set the encoding policy.
    #[inline]
    pub fn set_encode_policy(&mut self, encode_policy: EncodePolicy) {
        self.encode_policy = encode_policy;
    }
}

// original code from tosixel.rs
impl<W: IoWrite> SixelOutput<W> {
    /* GNU Screen penetration */

    /// Writes a segmented data packet to the output,
    /// wrapped with DCS (Device Control String) start and end sequences.
    ///
    /// Segments data according to `SCREEN_PACKET_SIZE`, splitting if necessary.
    fn penetrate(
        &mut self,
        nwrite: usize,   // output size
        dcs_start: &str, // DCS introducer
        dcs_end: &str,   // DCS terminato
    ) {
        let splitsize = SCREEN_PACKET_SIZE - dcs_start.len() - dcs_end.len();
        let mut pos = 0;
        while pos < nwrite {
            let _ = self.fn_write.write(dcs_start.as_bytes());
            let _ = self.fn_write.write(self.buffer[pos..pos + splitsize].as_bytes());
            let _ = self.fn_write.write(dcs_end.as_bytes());
            pos += splitsize;
        }
    }

    /// Manages buffer overflow by writing buffered data in packets of `PACKET_SIZE`.
    ///
    /// Uses `penetrate` if multiplexing is enabled; otherwise, writes directly to output.
    fn advance(&mut self) {
        if self.buffer.len() >= SixelOutput::<W>::PACKET_SIZE {
            if self.penetrate_multiplexer {
                self.penetrate(SixelOutput::<W>::PACKET_SIZE, DCS_START_7BIT, DCS_END_7BIT);
            } else {
                let _ =
                    self.fn_write.write(self.buffer[..SixelOutput::<W>::PACKET_SIZE].as_bytes());
            }
            self.buffer.drain(0..SixelOutput::<W>::PACKET_SIZE);
        }
    }

    /// Writes a single character to the output.
    #[inline]
    pub fn putc(&mut self, value: char) {
        self.buffer.push(value);
    }

    /// Writes a string to the output.
    #[inline]
    pub fn puts(&mut self, value: &str) {
        self.buffer.push_str(value);
    }

    /// Writes an integer value to the output as a string.
    #[inline]
    pub(crate) fn puti(&mut self, i: i32) {
        self.puts(format!("{}", i).as_str());
    }

    /// Writes a byte value to the output as a string.
    #[inline]
    #[expect(unused, reason = "…")]
    pub(crate) fn putb(&mut self, b: u8) {
        self.puts(format!("{}", b).as_str());
    }

    /// Adds a "flash" signal in the output stream.
    pub fn put_flash(&mut self) -> SixelResult<()> {
        if self.has_gri_arg_limit {
            /* VT240 Max 255 ? */
            while self.save_count > 255 {
                /* argument of DECGRI('!') is limitted to 255 in real VT */
                self.puts("!255");
                self.advance();
                self.putc(unsafe { char::from_u32_unchecked(self.save_pixel as u32) });
                self.advance();
                self.save_count -= 255;
            }
        }
        if self.save_count > 3 {
            /* DECGRI Graphics Repeat Introducer ! Pn Ch */
            self.putc('!');
            self.advance();
            self.puti(self.save_count);
            self.advance();
            self.putc(unsafe { char::from_u32_unchecked(self.save_pixel as u32) });
            self.advance();
        } else {
            for _ in 0..self.save_count {
                self.putc(unsafe { char::from_u32_unchecked(self.save_pixel as u32) });
                self.advance();
            }
        }
        self.save_pixel = 0;
        self.save_count = 0;
        Ok(())
    }

    /// Outputs a single pixel to the sixel stream.
    pub fn put_pixel(&mut self, mut pix: u8) -> SixelResult<()> {
        if pix > b'?' {
            pix = b'\0';
        }
        pix += b'?';
        if pix == self.save_pixel {
            self.save_count += 1;
        } else {
            self.put_flash()?;
            self.save_pixel = pix;
            self.save_count = 1;
        }
        Ok(())
    }

    /// Writes a sixel node to the output, with additional parameters for color and position.
    pub fn put_node(
        &mut self,     /* output context */
        x: &mut i32,   /* header position */
        np: SixelNode, /* node object */
        ncolors: i32,  /* number of palette colors */
        keycolor: i32,
    ) -> SixelResult<()> {
        if ncolors != 2 || keycolor == -1 {
            /* designate palette index */
            if self.active_palette != np.pal {
                self.putc('#');
                self.advance();
                self.puti(np.pal);
                self.advance();
                self.active_palette = np.pal;
            }
        }

        while *x < np.sx {
            if *x != keycolor {
                self.put_pixel(0)?;
            }
            *x += 1;
        }
        while *x < np.mx {
            if *x != keycolor {
                self.put_pixel(np.map[*x as usize])?;
            }
            *x += 1;
        }
        self.put_flash()?;
        Ok(())
    }

    /// Encodes and outputs the sixel image header with the specified width and height.
    pub fn encode_header(&mut self, width: i32, height: i32) -> SixelResult<()> {
        let p = [0, 0, 0];
        let mut pcount = 3;

        let use_raster_attributes = true;

        if !self.skip_dcs_envelope {
            if self.has_8bit_control {
                self.puts(DCS_START_8BIT);
                self.advance();
            } else {
                self.puts(DCS_START_7BIT);
                self.advance();
            }
        }

        if p[2] == 0 {
            pcount -= 1;
            if p[1] == 0 {
                pcount -= 1;
                if p[0] == 0 {
                    pcount -= 1;
                }
            }
        }

        if pcount > 0 {
            self.puti(p[0]);
            self.advance();
            if pcount > 1 {
                self.putc(';');
                self.advance();
                self.puti(p[1]);
                self.advance();
                if pcount > 2 {
                    self.putc(';');
                    self.advance();
                    self.puti(p[2]);
                    self.advance();
                }
            }
        }

        self.putc('q');
        self.advance();

        if use_raster_attributes {
            self.puts("\"1;1;");
            self.advance();
            self.puti(width);
            self.advance();
            self.putc(';');
            self.advance();
            self.puti(height);
            self.advance();
        }

        Ok(())
    }

    /// Outputs an RGB color palette definition.
    pub fn output_rgb_palette_definition(
        &mut self,
        palette: &[u8],
        n: i32,
        keycolor: i32,
    ) -> SixelResult<()> {
        if n != keycolor {
            /* DECGCI Graphics Color Introducer  # Pc ; Pu; Px; Py; Pz */
            self.putc('#');
            self.advance();
            self.puti(n);
            self.advance();
            self.puts(";2;");
            self.advance();
            self.puti((palette[n as usize * 3] as i32 * 100 + 127) / 255);
            self.advance();
            self.putc(';');
            self.advance();
            self.puti((palette[n as usize * 3 + 1] as i32 * 100 + 127) / 255);
            self.advance();
            self.putc(';');
            self.advance();
            self.puti((palette[n as usize * 3 + 2] as i32 * 100 + 127) / 255);
            self.advance();
        }
        Ok(())
    }

    /// Outputs an HLS color palette definition.
    pub fn output_hls_palette_definition(
        &mut self,
        palette: &[u8],
        n: i32,
        keycolor: i32,
    ) -> SixelResult<()> {
        if n != keycolor {
            let n = n as usize;
            let r = palette[n * 3 + 0] as i32;
            let g = palette[n * 3 + 1] as i32;
            let b = palette[n * 3 + 2] as i32;
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            let l = ((max + min) * 100 + 255) / 510;
            let mut h = 0;
            let mut s = 0;

            if max == min {
                // h = s = 0;
            } else {
                if l < 50 {
                    s = ((max - min) * 100) / (max + min);
                } else {
                    s = ((max - min) * 100) / ((255 - max) + (255 - min));
                }
                if r == max {
                    h = 120 + (g - b) * 60 / (max - min);
                } else if g == max {
                    h = 240 + (b - r) * 60 / (max - min);
                } else if r < g
                /* if b == max */
                {
                    h = 360 + (r - g) * 60 / (max - min);
                } else {
                    h = 0 + (r - g) * 60 / (max - min);
                }
            }
            /* DECGCI Graphics Color Introducer  # Pc ; Pu; Px; Py; Pz */
            self.putc('#');
            self.advance();
            self.puti(n as i32);
            self.advance();
            self.puts(";1;");
            self.advance();
            self.puti(h);
            self.advance();
            self.putc(';');
            self.advance();
            self.puti(l);
            self.advance();
            self.putc(';');
            self.advance();
            self.puti(s);
            self.advance();
        }
        Ok(())
    }

    /// Encodes the sixel image body, including pixel and color data.
    #[expect(clippy::too_many_arguments)]
    pub fn encode_body(
        &mut self,
        pixels: &[u8],
        width: i32,
        height: i32,
        palette: &[u8],
        ncolors: usize,
        keycolor: i32,
        bodyonly: bool,
        palstate: Option<&[i32]>,
    ) -> SixelResult<()> {
        if palette.is_empty() {
            return Err(SixelError::BadArgument);
        }
        let len = ncolors * width as usize;
        self.active_palette = -1;

        let mut map: Vec<u8> = vec![0; len];

        if !bodyonly && (ncolors != 2 || keycolor == (-1)) {
            if matches!(self.palette_type, PaletteType::Hls) {
                for n in 0..ncolors {
                    self.output_hls_palette_definition(palette, n as i32, keycolor)?;
                }
            } else {
                for n in 0..ncolors {
                    self.output_rgb_palette_definition(palette, n as i32, keycolor)?;
                }
            }
        }
        let mut i = 0;
        let mut fillable: bool;
        let mut pix;

        for y in 0..height {
            if self.encode_policy != EncodePolicy::Size {
                fillable = false;
            } else if palstate.is_some() {
                /* high color sixel */
                pix = pixels[((y - i) * width) as usize] as i32;
                fillable = pix as usize >= ncolors;
            } else {
                /* normal sixel */
                fillable = true;
            }
            for x in 0..width {
                if y > i32::MAX / width {
                    /* integer overflow */
                    /*sixel_helper_set_additional_message(
                    "sixel_encode_body: integer overflow detected."
                    " (y > INT_MAX)");*/
                    return Err(SixelError::BadIntegerOverflow);
                }
                let mut check_integer_overflow = y * width;
                if check_integer_overflow > i32::MAX - x {
                    /* integer overflow */
                    /*sixel_helper_set_additional_message(
                    "sixel_encode_body: integer overflow detected."
                    " (y * width > INT_MAX - x)");*/
                    return Err(SixelError::BadIntegerOverflow);
                }
                pix = pixels[(check_integer_overflow + x) as usize] as i32; /* color index */
                if pix >= 0 && (pix as usize) < ncolors && pix != keycolor {
                    if pix > i32::MAX / width {
                        /* integer overflow */
                        /*sixel_helper_set_additional_message(
                        "sixel_encode_body: integer overflow detected."
                        " (pix > INT_MAX / width)");*/
                        return Err(SixelError::BadIntegerOverflow);
                    }
                    check_integer_overflow = pix * width;
                    if check_integer_overflow > i32::MAX - x {
                        /* integer overflow */
                        /*sixel_helper_set_additional_message(
                        "sixel_encode_body: integer overflow detected."
                        " (pix * width > INT_MAX - x)");*/
                        return Err(SixelError::BadIntegerOverflow);
                    }
                    map[(pix * width + x) as usize] |= 1 << i;
                } else if palstate.is_none() {
                    fillable = false;
                }
            }

            i += 1;
            if i < 6 && (y + 1) < height {
                continue;
            }
            for c in 0..ncolors {
                let mut sx = 0;
                while sx < width {
                    if map[c * width as usize + sx as usize] == 0 {
                        sx += 1;
                        continue;
                    }
                    let mut mx = sx + 1;
                    while mx < width {
                        if map[c * width as usize + mx as usize] != 0 {
                            mx += 1;
                            continue;
                        }
                        let mut n = 1;
                        while (mx + n) < width {
                            if map[c * width as usize + mx as usize + n as usize] != 0 {
                                break;
                            }
                            n += 1;
                        }

                        if n >= 10 || (mx + n) >= width {
                            break;
                        }
                        mx = mx + n - 1;
                        mx += 1;
                    }
                    let np = SixelNode {
                        pal: c as i32,
                        sx,
                        mx,
                        map: map[c * width as usize..].to_vec(),
                    };

                    self.nodes.insert(0, np);
                    sx = mx - 1;
                    sx += 1;
                }
            }

            if y != 5 {
                /* DECGNL Graphics Next Line */
                self.putc('-');
                self.advance();
            }
            let mut x = 0;
            while let Some(mut np) = self.nodes.pop() {
                if x > np.sx {
                    /* DECGCR Graphics Carriage Return */
                    self.putc('$');
                    self.advance();
                    x = 0;
                }

                if fillable {
                    // memset(np->map + np->sx, (1 << i) - 1, (size_t)(np->mx - np->sx));
                    let v = (1 << i) - 1;
                    np.map.resize(np.mx as usize, v);
                    for j in np.sx..np.mx {
                        np.map[j as usize] = v;
                    }
                }
                self.put_node(&mut x, np, ncolors as i32, keycolor)?;

                let mut ni = self.nodes.len() as i32 - 1;
                while ni >= 0 {
                    let onode = &self.nodes[ni as usize];

                    if onode.sx < x {
                        ni -= 1;
                        continue;
                    }

                    if fillable {
                        // memset(np.map + np.sx, (1 << i) - 1, (size_t)(np.mx - np.sx));
                        let np = &mut self.nodes[ni as usize];
                        let v = (1 << i) - 1;
                        np.map.resize(np.mx as usize, v);
                        for j in np.sx..np.mx {
                            np.map[j as usize] = v;
                        }
                    }
                    let np = self.nodes.remove(ni as usize);
                    self.put_node(&mut x, np, ncolors as i32, keycolor)?;
                    ni -= 1;
                }

                fillable = false;
            }

            i = 0;
            map.clear();
            map.resize(len, 0);
        }

        if palstate.is_some() {
            self.putc('$');
            self.advance();
        }
        Ok(())
    }

    /// Encodes and outputs the sixel image footer.
    pub fn encode_footer(&mut self) -> SixelResult<()> {
        if !self.skip_dcs_envelope && !self.penetrate_multiplexer {
            if self.has_8bit_control {
                self.puts(DCS_END_8BIT);
                self.advance();
            } else {
                self.puts(DCS_END_7BIT);
                self.advance();
            }
        }

        /* flush buffer */
        if !self.buffer.is_empty() {
            if self.penetrate_multiplexer {
                self.penetrate(self.buffer.len(), DCS_START_7BIT, DCS_END_7BIT);
                let _ = self.fn_write.write(b"\x1B\\");
            } else {
                let _ = self.fn_write.write(self.buffer.as_bytes());
            }
        }
        Ok(())
    }

    /// Encodes a sixel dithered image with specified pixels and configuration.
    pub fn encode_dither(
        &mut self,
        pixels: &[u8],
        width: i32,
        height: i32,
        dither: &mut DitherConf,
    ) -> SixelResult<()> {
        let input_pixels = match dither.pixelformat {
            PixelFormat::PAL1
            | PixelFormat::PAL2
            | PixelFormat::PAL4
            | PixelFormat::G1
            | PixelFormat::G2
            | PixelFormat::G4 => {
                let mut paletted_pixels = vec![0; (width * height * 3) as usize];
                dither.pixelformat = sixel_helper_normalize_pixelformat(
                    &mut paletted_pixels,
                    pixels,
                    dither.pixelformat,
                    width,
                    height,
                )?;
                paletted_pixels
            }

            PixelFormat::PAL8 | PixelFormat::G8 | PixelFormat::GA88 | PixelFormat::AG88 => {
                pixels.to_vec()
            }

            _ => {
                /* apply palette */
                dither.apply_palette(pixels, width, height)?
            }
        };
        self.encode_header(width, height)?;
        self.encode_body(
            &input_pixels,
            width,
            height,
            &dither.palette,
            dither.ncolors as usize,
            dither.keycolor,
            dither.bodyonly,
            None,
        )?;
        self.encode_footer()?;
        Ok(())
    }

    /// Encodes a high-color sixel image.
    pub fn encode_highcolor(
        &mut self,
        pixels: &mut [u8],
        width: i32,
        mut height: i32,
        dither: &mut DitherConf,
    ) -> SixelResult<()> {
        let maxcolors = 1 << 15;
        let mut px_idx = 0;
        let mut normalized_pixels = vec![0; (width * height * 3) as usize];
        let pixels = if !matches!(dither.pixelformat, PixelFormat::BGR888) {
            /* normalize pixelfromat */
            sixel_helper_normalize_pixelformat(
                &mut normalized_pixels,
                pixels,
                dither.pixelformat,
                width,
                height,
            )?;
            &mut normalized_pixels
        } else {
            pixels
        };
        let mut paletted_pixels: Vec<u8> = vec![0; (width * height) as usize];
        let mut rgbhit = vec![0; maxcolors as usize];
        let mut rgb2pal = vec![0; maxcolors as usize];
        // let marks = &mut rgb2pal[maxcolors as usize..];
        let mut output_count = 0;

        let mut is_running = true;
        let mut palstate: Vec<i32> = vec![0; SIXEL_PALETTE_MAX];
        let mut palhitcount: Vec<i32> = vec![0; SIXEL_PALETTE_MAX];
        let mut marks = vec![false; (width * 6) as usize];
        while is_running {
            let mut dst = 0;
            let mut nextpal: usize = 0;
            let mut threshold = 1;
            let mut dirty = false;
            let mut mptr = 0;
            marks.clear();
            marks.resize((width * 6) as usize, false);
            palstate.clear();
            palstate.resize(SIXEL_PALETTE_MAX, 0);
            let mut y = 0;
            let mut mod_y = 0;

            loop {
                for x in 0..width {
                    if marks[mptr] {
                        paletted_pixels[dst] = 255;
                    } else {
                        sixel_apply_15bpp_dither(
                            &mut pixels[px_idx..],
                            x,
                            y,
                            width,
                            height,
                            dither.method_for_diffuse,
                        );
                        let pix = ((pixels[px_idx] & 0xf8) as i32) << 7
                            | ((pixels[px_idx + 1] & 0xf8) as i32) << 2
                            | ((pixels[px_idx + 2] >> 3) & 0x1f) as i32;

                        if rgbhit[pix as usize] == 0 {
                            loop {
                                if nextpal >= 255 {
                                    if threshold >= 255 {
                                        break;
                                    } else {
                                        threshold = if threshold == 1 { 9 } else { 255 };
                                        nextpal = 0;
                                    }
                                } else if palstate[nextpal] != 0 || palhitcount[nextpal] > threshold
                                {
                                    nextpal += 1;
                                } else {
                                    break;
                                }
                            }

                            if nextpal >= 255 {
                                dirty = true;
                                paletted_pixels[dst] = 255;
                            } else {
                                let pal = nextpal * 3;

                                rgbhit[pix as usize] = 1;
                                if output_count > 0 {
                                    rgbhit[((dither.palette[pal] as usize & 0xf8) << 7)
                                        | ((dither.palette[pal + 1] as usize & 0xf8) << 2)
                                        | ((dither.palette[pal + 2] as usize >> 3) & 0x1f)] = 0;
                                }
                                paletted_pixels[dst] = nextpal as u8;
                                rgb2pal[pix as usize] = nextpal as u8;
                                nextpal += 1;
                                marks[mptr] = true;
                                palstate[paletted_pixels[dst] as usize] = PALETTE_CHANGE;
                                palhitcount[paletted_pixels[dst] as usize] = 1;
                                dither.palette[pal] = pixels[px_idx + 0];
                                dither.palette[pal + 1] = pixels[px_idx + 1];
                                dither.palette[pal + 2] = pixels[px_idx + 2];
                            }
                        } else {
                            let pp = rgb2pal[pix as usize];
                            paletted_pixels[dst] = pp;
                            let pp = pp as usize;

                            marks[mptr] = true;
                            if palstate[pp] != 0 {
                                palstate[pp] = PALETTE_HIT;
                            }
                            if palhitcount[pp] < 255 {
                                palhitcount[pp] += 1;
                            }
                        }
                    }

                    mptr += 1;
                    dst += 1;
                    px_idx += 3;
                }
                y += 1;
                if y >= height {
                    if dirty {
                        mod_y = 5;
                    } else {
                        is_running = false;
                        break;
                    }
                }
                if dirty && (mod_y == 5 || y >= height) {
                    let orig_height = height;

                    if output_count == 0 {
                        self.encode_header(width, height)?;
                    }
                    output_count += 1;

                    height = y;

                    self.encode_body(
                        &paletted_pixels,
                        width,
                        height,
                        &dither.palette,
                        dither.ncolors as usize,
                        255,
                        dither.bodyonly,
                        Some(&palstate),
                    )?;
                    if y >= orig_height {
                        // end outer loop
                        is_running = false;
                        break;
                    }
                    px_idx -= (6 * width * 3) as usize;
                    height = orig_height - height + 6;
                    break; // goto next outer loop
                }
                mod_y += 1;
                if mod_y == 6 {
                    marks.clear();
                    marks.resize(maxcolors as usize, false);
                    mptr = 0;
                    mod_y = 0;
                }
            }
        }
        if output_count == 0 {
            self.encode_header(width, height)?;
        }

        let _ = self.encode_body(
            &paletted_pixels,
            width,
            height,
            &dither.palette,
            dither.ncolors as usize,
            255,
            dither.bodyonly,
            Some(&palstate),
        );

        let _ = self.encode_footer();

        Ok(())
    }

    /// Encodes a sixel image with dither and color depth settings.
    pub fn encode(
        &mut self,
        pixels: &mut [u8],
        width: i32,
        height: i32,
        _depth: i32, /* color depth */
        dither: &mut DitherConf,
    ) -> SixelResult<()> /* output context */ {
        /*
            println!("sixel_encode: {} x {} depth {}", width, height, _depth);
            println!("dither:");
            println!("\treqcolors: {}", dither.reqcolors);
            println!("\tncolors: {}", dither.ncolors);
            println!("\torigcolors: {}", dither.origcolors);
            println!("\toptimized: {}", dither.optimized);
            println!("\toptimize_palette: {}", dither.optimize_palette);
            println!("\tcomplexion: {}", dither.complexion);
            println!("\tbodyonly: {}", dither.bodyonly);
            println!("\tmethod_for_largest: {:?}", dither.method_for_largest as i32);
            println!("\tmethod_for_rep: {:?}", dither.method_for_rep as i32);
            println!("\tmethod_for_diffuse: {:?}", dither.method_for_diffuse as i32);
            println!("\tquality_mode: {:?}", dither.quality_mode as i32);
            println!("\tkeycolor: {:?}", dither.keycolor);
            println!("\tpixelformat: {:?}", dither.pixelformat as i32);
        */
        if width < 1 {
            return Err(SixelError::BadInput);
            /*
            sixel_helper_set_additional_message(
                "sixel_encode: bad width parameter."
                " (width < 1)");
            status = SIXEL_BAD_INPUT;
            goto end;*/
        }

        if height < 1 {
            return Err(SixelError::BadInput);
            /*
            sixel_helper_set_additional_message(
                "sixel_encode: bad height parameter."
                " (height < 1)");
            status = SIXEL_BAD_INPUT;
            goto end;*/
        }
        match dither.quality_mode {
            crate::Quality::Auto
            | crate::Quality::High
            | crate::Quality::Low
            | crate::Quality::Full => {
                self.encode_dither(pixels, width, height, dither)?;
            }
            crate::Quality::HighColor => {
                self.encode_highcolor(pixels, width, height, dither)?;
            }
        }
        Ok(())
    }
}
