//! Generated square cover art, identical in every prototype.

pub const ARTWORK_VARIANT_COUNT: usize = 36;
pub const ARTWORK_SIZE_PIXELS: u32 = 320;
const GOLDEN_ANGLE_DEGREES: f32 = 137.507_77;

fn hsl_to_rgb_f32(hue_degrees: f32, saturation: f32, lightness: f32) -> [f32; 3] {
    let hue = hue_degrees.rem_euclid(360.0);
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let secondary = chroma * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let offset = lightness - chroma / 2.0;
    let (red, green, blue) = match (hue / 60.0) as u32 {
        0 => (chroma, secondary, 0.0),
        1 => (secondary, chroma, 0.0),
        2 => (0.0, chroma, secondary),
        3 => (0.0, secondary, chroma),
        4 => (secondary, 0.0, chroma),
        _ => (chroma, 0.0, secondary),
    };
    [red + offset, green + offset, blue + offset]
}

fn mix(from: [f32; 3], to: [f32; 3], amount: f32) -> [f32; 3] {
    let amount = amount.clamp(0.0, 1.0);
    [
        from[0] + (to[0] - from[0]) * amount,
        from[1] + (to[1] - from[1]) * amount,
        from[2] + (to[2] - from[2]) * amount,
    ]
}

fn smoothstep(edge_start: f32, edge_end: f32, value: f32) -> f32 {
    let t = ((value - edge_start) / (edge_end - edge_start)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Generated square cover art, RGBA8, `ARTWORK_SIZE_PIXELS` wide and high.
/// Same bytes in every prototype so the frameworks are compared on identical pictures.
pub fn artwork_rgba(variant: usize) -> Vec<u8> {
    let size = ARTWORK_SIZE_PIXELS as usize;
    let hue = (variant as f32 * GOLDEN_ANGLE_DEGREES) % 360.0;
    let top_left = hsl_to_rgb_f32(hue, 0.55, 0.46);
    let bottom_right = hsl_to_rgb_f32(hue + 38.0, 0.62, 0.14);
    let highlight = hsl_to_rgb_f32(hue + 28.0, 0.80, 0.68);
    let counter = hsl_to_rgb_f32(hue + 180.0, 0.45, 0.58);
    let mut noise_state: u32 = 0x9E37_79B9 ^ (variant as u32).wrapping_mul(0x85EB_CA6B);
    let mut pixels = Vec::with_capacity(size * size * 4);
    for row in 0..size {
        for column in 0..size {
            let x = (column as f32 + 0.5) / size as f32;
            let y = (row as f32 + 0.5) / size as f32;
            let mut color = mix(top_left, bottom_right, (x + y) * 0.5);
            match variant % 6 {
                0 => {
                    let distance = ((x - 0.68).powi(2) + (y - 0.30).powi(2)).sqrt();
                    let glow = (1.0 - distance / 0.62).max(0.0).powi(2);
                    color = mix(color, highlight, glow * 0.75);
                }
                1 => {
                    let distance = ((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt();
                    let disc = 1.0 - smoothstep(0.455, 0.462, distance);
                    let groove = 0.5 + 0.5 * (distance * 260.0).sin();
                    color = mix(color, [0.04, 0.04, 0.05], disc * (0.82 + 0.06 * groove));
                    let label = 1.0 - smoothstep(0.125, 0.131, distance);
                    color = mix(color, counter, label);
                    let spindle = 1.0 - smoothstep(0.012, 0.018, distance);
                    color = mix(color, [0.06, 0.06, 0.06], spindle);
                }
                2 => {
                    let sky = mix(highlight, top_left, y * 1.4);
                    color = mix(sky, color, smoothstep(0.62, 0.66, y));
                    let distance = ((x - 0.5).powi(2) + (y - 0.56).powi(2)).sqrt();
                    let sun = (1.0 - smoothstep(0.205, 0.212, distance))
                        * (1.0 - smoothstep(0.62, 0.625, y));
                    color = mix(color, [0.98, 0.93, 0.82], sun);
                }
                3 => {
                    for (center_x, center_y, radius, tint) in
                        [(0.25, 0.72, 0.45, counter), (0.78, 0.22, 0.38, highlight)]
                    {
                        let distance = ((x - center_x).powi(2) + (y - center_y).powi(2)).sqrt();
                        let glow = (1.0 - distance / radius).max(0.0).powi(2);
                        color = mix(color, tint, glow * 0.6);
                    }
                }
                4 => {
                    for (center_x, center_y, radius, tint) in [
                        (0.36, 0.40, 0.26, highlight),
                        (0.62, 0.52, 0.22, counter),
                        (0.46, 0.68, 0.16, [0.95, 0.93, 0.88]),
                    ] {
                        let distance = ((x - center_x).powi(2) + (y - center_y).powi(2)).sqrt();
                        let inside = 1.0 - smoothstep(radius - 0.004, radius, distance);
                        color = mix(color, tint, inside * 0.88);
                    }
                }
                _ => {
                    let flat = mix(top_left, highlight, 0.25);
                    color = mix(flat, bottom_right, smoothstep(0.615, 0.62, y));
                    let rule = (1.0 - smoothstep(0.0, 0.004, (y - 0.585).abs()))
                        * smoothstep(0.075, 0.08, x)
                        * (1.0 - smoothstep(0.70, 0.705, x));
                    color = mix(color, [0.97, 0.95, 0.90], rule);
                }
            }
            noise_state ^= noise_state << 13;
            noise_state ^= noise_state >> 17;
            noise_state ^= noise_state << 5;
            let grain = (noise_state % 7) as f32 - 3.0;
            for channel in color {
                pixels.push((channel * 255.0 + grain).round().clamp(0.0, 255.0) as u8);
            }
            pixels.push(255);
        }
    }
    pixels
}
