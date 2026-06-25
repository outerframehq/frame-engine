// A tiny hand-rolled bitmap font for drawing debug text into the pixel buffer.
//
// Each glyph is a 5-wide x 7-tall grid, stored as 7 bytes (one byte per row).
// In each byte we use only the low 5 bits: bit 4 (0b10000) is the LEFTMOST
// column, bit 0 (0b00001) is the rightmost. A set bit = a lit pixel.
//
// We only define the characters the inspector actually prints (digits, a few
// uppercase letters for the labels, and some punctuation). Anything else —
// including space — renders blank.

const GLYPH_WIDTH: i32 = 5;
const GLYPH_HEIGHT: i32 = 7;

// The 7-row bitmap for a character. Unknown chars render blank.
fn glyph(c: char) -> [u8; 7] {
    match c {
        '0' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x0E, 0x11, 0x01, 0x06, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x0E, 0x11, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x11, 0x0E],

        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],

        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x06],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x06, 0x06, 0x08],
        '-' => [0x00, 0x00, 0x00, 0x0E, 0x00, 0x00, 0x00],

        _ => [0x00; 7], // space and anything undefined: blank
    }
}

// Draw one character with its top-left at (x, y); each font pixel becomes a
// scale x scale block on screen.
fn draw_char(
    buffer: &mut [u32],
    width_px: u32,
    height_px: u32,
    x: i32,
    y: i32,
    c: char,
    scale: i32,
    color: u32,
) {
    let rows = glyph(c);
    for (row_index, &row_bits) in rows.iter().enumerate() {
        for col in 0..GLYPH_WIDTH {
            // bit 4 is the leftmost column, so shift down from the high end
            let lit = (row_bits >> (GLYPH_WIDTH - 1 - col)) & 1 == 1;
            if !lit {
                continue;
            }
            // paint this font pixel as a scale x scale block
            for sy in 0..scale {
                for sx in 0..scale {
                    let px = x + col * scale + sx;
                    let py = y + row_index as i32 * scale + sy;
                    if px >= 0 && px < width_px as i32 && py >= 0 && py < height_px as i32 {
                        let index = py as u32 * width_px + px as u32;
                        buffer[index as usize] = color;
                    }
                }
            }
        }
    }
}

// Draw a string starting at (x, y). Supports '\n' for new lines.
pub fn draw_text(
    buffer: &mut [u32],
    width_px: u32,
    height_px: u32,
    x: i32,
    y: i32,
    text: &str,
    scale: i32,
    color: u32,
) {
    let mut cursor_x = x;
    let mut cursor_y = y;
    for c in text.chars() {
        if c == '\n' {
            cursor_x = x;
            cursor_y += (GLYPH_HEIGHT + 1) * scale; // one blank row between lines
            continue;
        }
        draw_char(
            buffer, width_px, height_px, cursor_x, cursor_y, c, scale, color,
        );
        cursor_x += (GLYPH_WIDTH + 1) * scale; // one blank column between chars
    }
}
