#![forbid(unsafe_code)]

//! `cargo run -p xtask --bin ppm2png -- <input.ppm> [output.png]` —
//! convert the P6 PPM dumps written by the render-gate scratch-dump
//! tests (`render_gate.rs`, `render_dump_extra.rs`, `hud_dump.rs`, all
//! `#[ignore]`d) into PNG for eyeballing. The output path defaults to
//! the input path with the extension swapped for `.png`.
//!
//! Every dumper in the tree writes the same shape — `P6 {w} {h} 255\n`
//! followed by w*h*3 RGB bytes — so the parser is deliberately strict:
//! binary P6 only (P3/P5 are errors), maxval 255 only, and the raster
//! must be exactly w*h*3 bytes. A file that violates that is a bug in
//! the dumper, not something to paper over with a guess.

use std::path::{Path, PathBuf};
use std::process::exit;

/// Header whitespace per the netpbm spec: space, TAB, CR, LF, VT, FF.
/// `u8::is_ascii_whitespace` omits VT, so this is its own predicate.
fn is_ppm_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
}

/// Advance `i` past whitespace and `#`-to-end-of-line comments (the
/// newline terminating a comment is consumed as whitespace on the next
/// iteration).
fn skip_ws_and_comments(bytes: &[u8], i: &mut usize) {
    while *i < bytes.len() {
        if bytes[*i] == b'#' {
            while *i < bytes.len() && bytes[*i] != b'\n' {
                *i += 1;
            }
        } else if is_ppm_ws(bytes[*i]) {
            *i += 1;
        } else {
            break;
        }
    }
}

/// One header field: skip whitespace/comments, then read ASCII decimal
/// digits. `field` names the token ("width", "height", "maxval") for
/// error messages. Checked arithmetic — a header claiming a dimension
/// that does not fit u32 is a corrupt file, not a wrap-around.
fn parse_decimal(bytes: &[u8], i: &mut usize, field: &str) -> Result<u32, String> {
    skip_ws_and_comments(bytes, i);
    let start = *i;
    while *i < bytes.len() && bytes[*i].is_ascii_digit() {
        *i += 1;
    }
    let digits = &bytes[start..*i];
    if digits.is_empty() {
        return Err(format!("expected {field}, found no decimal digits"));
    }
    let mut value: u32 = 0;
    for &d in digits {
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(u32::from(d - b'0')))
            .ok_or_else(|| format!("{field} exceeds the 32-bit range"))?;
    }
    Ok(value)
}

/// Parse a binary P6 PPM and return `(width, height, pixels)` where
/// `pixels` is the w*h*3-byte RGB raster, row-major. Pure function on a
/// byte slice so the tests need no files.
fn parse_p6(bytes: &[u8]) -> Result<(u32, u32, &[u8]), String> {
    if !bytes.starts_with(b"P6") {
        let magic = String::from_utf8_lossy(&bytes[..bytes.len().min(2)]);
        return Err(format!(
            "not a binary P6 PPM (magic {magic:?}); P3/P5 and other variants are unsupported"
        ));
    }
    let mut i = 2;
    let width = parse_decimal(bytes, &mut i, "width")?;
    let height = parse_decimal(bytes, &mut i, "height")?;
    let maxval = parse_decimal(bytes, &mut i, "maxval")?;
    if width == 0 || height == 0 {
        return Err(format!(
            "degenerate raster {width}x{height}: dimensions must be positive"
        ));
    }
    if maxval != 255 {
        return Err(format!(
            "unsupported maxval {maxval}: only 255 (8-bit samples) is accepted"
        ));
    }
    // Exactly one whitespace byte separates maxval from the raster —
    // the `\n` every dumper here writes.
    match bytes.get(i) {
        None => return Err("file ends after maxval, missing raster".to_string()),
        Some(b) if is_ppm_ws(*b) => i += 1,
        Some(b) => {
            return Err(format!(
                "expected a single whitespace byte after maxval, found {b:#04x}"
            ));
        }
    }
    let expected = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|px| px.checked_mul(3))
        .ok_or_else(|| format!("{width}x{height} raster size overflows"))?;
    let actual = (bytes.len() - i) as u64;
    if actual != expected {
        let what = if actual > expected {
            "trailing garbage after raster"
        } else {
            "truncated pixel data"
        };
        return Err(format!(
            "{what}: expected {expected} bytes for {width}x{height} RGB, found {actual}"
        ));
    }
    Ok((width, height, &bytes[i..]))
}

fn run(input: &Path, output: &Path) -> Result<(), String> {
    let bytes = std::fs::read(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let (w, h, pixels) = parse_p6(&bytes).map_err(|e| format!("{}: {e}", input.display()))?;
    let img = image::RgbImage::from_raw(w, h, pixels.to_vec()).ok_or_else(|| {
        format!(
            "{w}x{h} does not match the {} raster bytes the parser returned",
            pixels.len()
        )
    })?;
    let file = std::fs::File::create(output).map_err(|e| format!("{}: {e}", output.display()))?;
    // write_to wants a buffered writer (the PNG encoder seeks).
    img.write_to(&mut std::io::BufWriter::new(file), image::ImageFormat::Png)
        .map_err(|e| format!("{}: {e}", output.display()))?;
    println!("wrote {} ({w}x{h})", output.display());
    Ok(())
}

fn main() {
    // Strict positional args — a third argument is a misuse, not
    // something to silently drop (same stance as gen_biome_maps).
    let mut args = std::env::args().skip(1);
    let (input, output): (PathBuf, PathBuf) = match (args.next(), args.next(), args.next()) {
        (Some(input), None, None) => {
            let output = Path::new(&input).with_extension("png");
            (PathBuf::from(input), output)
        }
        (Some(input), Some(output), None) => (PathBuf::from(input), PathBuf::from(output)),
        _ => {
            eprintln!("usage: ppm2png <input.ppm> [output.png]\nexpected 1 or 2 arguments");
            exit(2);
        }
    };
    if let Err(e) = run(&input, &output) {
        eprintln!("ppm2png: {e}");
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::parse_p6;

    /// Minimal valid P6: 2x1, one red pixel then one blue.
    fn tiny_ppm() -> Vec<u8> {
        let mut buf = b"P6 2 1 255\n".to_vec();
        buf.extend_from_slice(&[255, 0, 0, 0, 0, 255]);
        buf
    }

    #[test]
    fn valid_p6_parses_to_dims_and_pixels() {
        let buf = tiny_ppm();
        let Ok((w, h, px)) = parse_p6(&buf) else {
            panic!("a minimal valid P6 must parse");
        };
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, &[255, 0, 0, 0, 0, 255]);
    }

    #[test]
    fn header_comments_and_whitespace_parse() {
        let mut buf = b"P6 # comment\n\t 2 # width\n 1 255\n".to_vec();
        buf.extend_from_slice(&[255, 0, 0, 0, 0, 255]);
        let Ok((w, h, px)) = parse_p6(&buf) else {
            panic!("header comments are legal PPM and must parse");
        };
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, &[255, 0, 0, 0, 0, 255]);
    }

    #[test]
    fn ascii_p3_magic_is_rejected() {
        let mut buf = b"P3 2 1 255\n".to_vec();
        buf.extend_from_slice(&[255, 0, 0, 0, 0, 255]);
        let Err(e) = parse_p6(&buf) else {
            panic!("P3 (ASCII) PPM must be rejected");
        };
        assert!(
            e.contains("P6"),
            "error should name the expected magic: {e}"
        );
    }

    #[test]
    fn truncated_pixel_data_is_rejected() {
        let mut buf = tiny_ppm();
        buf.pop(); // one byte short of 2*1*3
        let Err(e) = parse_p6(&buf) else {
            panic!("short raster must be rejected");
        };
        assert!(e.contains("truncated"), "{e}");
    }

    #[test]
    fn trailing_garbage_is_rejected() {
        let mut buf = tiny_ppm();
        buf.push(0); // one byte past 2*1*3
        let Err(e) = parse_p6(&buf) else {
            panic!("trailing byte after the raster must be rejected");
        };
        assert!(e.contains("trailing"), "{e}");
    }

    #[test]
    fn non_255_maxval_is_rejected() {
        // 12 bytes: the correct raster size for 2x1 *16-bit* samples —
        // proves the rejection is the maxval, not the byte count.
        let mut buf = b"P6 2 1 65535\n".to_vec();
        buf.extend_from_slice(&[0; 12]);
        let Err(e) = parse_p6(&buf) else {
            panic!("maxval != 255 must be rejected");
        };
        assert!(e.contains("maxval"), "{e}");
    }
}
