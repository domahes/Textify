//! Application artwork: a window frame with a text cursor over it.
//!
//! The source is the line-only SVG supplied for the project, filled with the
//! even-odd rule and supersampled so small icons stay crisp. The module is
//! shared by the runtime tray icon and by the `gen_icon` binary, which bakes
//! the same artwork into `assets/TextifyRS.ico` for the executable resource.

/// Rasterizes the icon into `size x size` pixels.
///
/// Each pixel is a `u32` of the form `(a << 24) | (r << 16) | (g << 8) | b`
/// with straight (non-premultiplied) alpha; in little-endian memory this is
/// exactly the BGRA layout expected by DIB sections and ICO bitmap entries.
pub fn rasterize(size: usize) -> Vec<u32> {
    let shapes = icon_shapes();
    let samples = 4;
    let ink = 0x202124u32;
    let mut pixels = vec![0u32; size * size];

    for y in 0..size {
        for x in 0..size {
            let mut hits = 0;
            for sy in 0..samples {
                for sx in 0..samples {
                    let px = x as f32 + (sx as f32 + 0.5) / samples as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / samples as f32;
                    let (vx, vy) = icon_to_viewbox(px, py, size);
                    if shape_covers(&shapes, vx, vy) {
                        hits += 1;
                    }
                }
            }

            let alpha = (hits * 255 / (samples * samples)) as u32;
            // ink is already 0xRRGGBB; shifting it would corrupt the alpha byte.
            pixels[y * size + x] = (alpha << 24) | ink;
        }
    }

    pixels
}

fn icon_to_viewbox(px: f32, py: f32, size: usize) -> (f32, f32) {
    let inset = size as f32 * 0.08;
    let scale = (size as f32 - inset * 2.0) / 40.0;
    ((px - inset) / scale, (py - inset) / scale)
}

fn icon_shapes() -> Vec<Vec<(f32, f32)>> {
    let cursor = "M17.237,30.479l-6.052-17.293l17.293,6.052l-6.054,2.231l5.282,5.281\
                  l-2.957,2.957l-5.282-5.282L17.237,30.479z\
                  M19.071,22.612l5.679,5.681l1.543-1.543l-5.681-5.68l4.909-1.809\
                  l-12.707-4.447l4.447,12.707L19.071,22.612z";
    let window = "M1,5v30h38V5H1z M38,34H2V10h36V34z M38,9H2V6h36V9z\
                  M5,8H4V7h1V8z M7,8H6V7h1V8z M9,8H8V7h1V8z";
    let mut shapes = parse_svg_path(cursor);
    shapes.extend(parse_svg_path(window));
    shapes
}

pub fn shape_covers(shapes: &[Vec<(f32, f32)>], x: f32, y: f32) -> bool {
    let mut crossings = 0i32;
    for shape in shapes {
        let count = shape.len();
        if count < 2 {
            continue;
        }
        for index in 0..count {
            let (x0, y0) = shape[index];
            let (x1, y1) = shape[(index + 1) % count];
            if (y0 > y) == (y1 > y) {
                continue;
            }
            let cross = x0 + (y - y0) * (x1 - x0) / (y1 - y0);
            if x < cross {
                crossings += 1;
            }
        }
    }
    crossings % 2 == 1
}

/// Parses the line commands this icon actually uses: M/L/H/V and l/h/v, plus z.
fn parse_svg_path(source: &str) -> Vec<Vec<(f32, f32)>> {
    let bytes = source.as_bytes();
    let mut index = 0usize;
    let mut shapes = Vec::new();
    let mut current: Vec<(f32, f32)> = Vec::new();
    let mut cursor = (0.0f32, 0.0f32);
    let mut start = cursor;
    let mut command = b'M';

    let finish = |shapes: &mut Vec<Vec<(f32, f32)>>, current: &mut Vec<(f32, f32)>| {
        if current.len() >= 3 {
            shapes.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };

    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        if bytes[index].is_ascii_alphabetic() {
            command = bytes[index];
            index += 1;
        }

        match command {
            b'M' | b'L' => {
                if command == b'M' {
                    finish(&mut shapes, &mut current);
                }
                let (x, y, next) = read_pair(bytes, index);
                index = next;
                cursor = (x, y);
                if command == b'M' {
                    start = cursor;
                }
                current.push(cursor);
                command = if command == b'M' { b'L' } else { command };
            }
            b'l' => {
                let (dx, dy, next) = read_pair(bytes, index);
                index = next;
                cursor = (cursor.0 + dx, cursor.1 + dy);
                current.push(cursor);
            }
            b'H' => {
                let (x, next) = read_number(bytes, index);
                index = next;
                cursor.0 = x;
                current.push(cursor);
            }
            b'h' => {
                let (dx, next) = read_number(bytes, index);
                index = next;
                cursor.0 += dx;
                current.push(cursor);
            }
            b'V' => {
                let (y, next) = read_number(bytes, index);
                index = next;
                cursor.1 = y;
                current.push(cursor);
            }
            b'v' => {
                let (dy, next) = read_number(bytes, index);
                index = next;
                cursor.1 += dy;
                current.push(cursor);
            }
            b'Z' | b'z' => {
                cursor = start;
                finish(&mut shapes, &mut current);
            }
            _ => break,
        }
    }
    finish(&mut shapes, &mut current);
    shapes
}

fn read_pair(bytes: &[u8], index: usize) -> (f32, f32, usize) {
    let (x, index) = read_number(bytes, index);
    let (y, index) = read_number(bytes, index);
    (x, y, index)
}

fn read_number(bytes: &[u8], mut index: usize) -> (f32, usize) {
    while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b',') {
        index += 1;
    }
    let start = index;
    if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
        index += 1;
    }
    while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
        index += 1;
    }
    let text = std::str::from_utf8(&bytes[start..index]).unwrap_or("0");
    (text.parse::<f32>().unwrap_or(0.0), index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_window_frame_and_its_buttons() {
        let shapes = icon_shapes();
        assert_eq!(shapes.len(), 8, "cursor 2 + window 6 subpaths");
        assert!(!shape_covers(&shapes, 0.5, 0.5), "corner stays empty");
        assert!(shape_covers(&shapes, 1.5, 20.0), "left frame is drawn");
        assert!(shape_covers(&shapes, 4.5, 7.5), "title button is drawn");
        assert!(!shape_covers(&shapes, 30.0, 20.0), "window body stays open");
    }

    #[test]
    fn rasterizes_transparent_corners_and_an_inked_frame() {
        let pixels = rasterize(32);
        assert_eq!(pixels.len(), 32 * 32);
        assert_eq!(pixels[0] >> 24, 0, "top-left corner is transparent");

        // With an 8% inset the left window frame (view box x=1..2) lands on
        // output column 3; row 16 is vertically centered.
        let frame = pixels[16 * 32 + 3];
        // The frame is narrower than one output pixel at 32px, so
        // supersampling legitimately gives partial coverage here.
        assert!(frame >> 24 > 100, "frame pixel is mostly opaque: {frame:#x}");
        assert_eq!(frame & 0x00FFFFFF, 0x202124, "frame uses the ink color");
    }
}
