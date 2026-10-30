//! The colors of the current-track page, taken from the artwork. Computed once per track, from
//! the cover already decoded for the LCD: nothing animates or blurs the artwork itself.

/// Pixels skipped between two samples: a 512-pixel cover needs no more to give its color.
const SAMPLE_STEP: usize = 7;
/// Bits kept of each channel, so close shades count as one color.
const BUCKET_BITS: u32 = 4;
/// How much more a fully saturated pixel counts than a grey one: Apple's backgrounds come from
/// the cover's color, not from its black borders.
const SATURATION_WEIGHT: u32 = 8;
/// Below and above these levels a pixel is a border, not a color.
const DARKEST_CHANNEL: u8 = 24;
const LIGHTEST_CHANNEL: u8 = 232;
/// Brightness of the top and bottom of the page: dark enough for white text, whatever the cover.
const TOP_BRIGHTNESS: f32 = 0.40;
const BOTTOM_BRIGHTNESS: f32 = 0.13;
/// Saturation ceiling, so a vivid cover stays a quiet background.
const MAXIMUM_SATURATION: f32 = 0.62;

/// The page's gradient, top then bottom, as (red, green, blue).
pub(crate) type PageTint = ([u8; 3], [u8; 3]);

/// The dominant color of `rgba` pixels (4 bytes each), favoring saturated ones; `None` when
/// there are no pixels.
pub(crate) fn dominant_color(rgba: &[u8]) -> Option<[u8; 3]> {
    let bucket_count = 1usize << (3 * BUCKET_BITS);
    let mut buckets = vec![(0u32, [0u32; 3]); bucket_count];
    for &[red, green, blue, _] in rgba.as_chunks::<4>().0.iter().step_by(SAMPLE_STEP) {
        let (lightest, darkest) = (red.max(green).max(blue), red.min(green).min(blue));
        let is_border = lightest < DARKEST_CHANNEL || darkest > LIGHTEST_CHANNEL;
        let weight = if is_border {
            1
        } else {
            1 + SATURATION_WEIGHT * u32::from(lightest - darkest)
        };
        let index = [red, green, blue].iter().fold(0usize, |index, channel| {
            (index << BUCKET_BITS) | usize::from(channel >> (8 - BUCKET_BITS))
        });
        let (bucket_weight, totals) = &mut buckets[index];
        *bucket_weight += weight;
        for (total, channel) in totals.iter_mut().zip([red, green, blue]) {
            *total += weight * u32::from(channel);
        }
    }
    let (weight, totals) = buckets.into_iter().max_by_key(|(weight, _)| *weight)?;
    (weight > 0).then(|| totals.map(|total| (total / weight) as u8))
}

/// The page's gradient for a cover of `color`: same hue, quieter and darker.
pub(crate) fn page_tint(color: [u8; 3]) -> PageTint {
    let (hue, saturation) = hue_and_saturation(color);
    let saturation = saturation.min(MAXIMUM_SATURATION);
    (
        from_hsv(hue, saturation, TOP_BRIGHTNESS),
        from_hsv(hue, saturation, BOTTOM_BRIGHTNESS),
    )
}

/// Hue in degrees and saturation (0 to 1) of an RGB color.
fn hue_and_saturation([red, green, blue]: [u8; 3]) -> (f32, f32) {
    let [red, green, blue] = [red, green, blue].map(|channel| f32::from(channel) / 255.0);
    let lightest = red.max(green).max(blue);
    let range = lightest - red.min(green).min(blue);
    if range <= f32::EPSILON {
        return (0.0, 0.0);
    }
    let sector = if lightest == red {
        ((green - blue) / range).rem_euclid(6.0)
    } else if lightest == green {
        (blue - red) / range + 2.0
    } else {
        (red - green) / range + 4.0
    };
    (sector * 60.0, range / lightest)
}

/// The RGB color of a hue (degrees), saturation and value (0 to 1).
fn from_hsv(hue: f32, saturation: f32, value: f32) -> [u8; 3] {
    let channel = |offset: f32| {
        let position = (offset + hue / 60.0).rem_euclid(6.0);
        let ramp = position.min(4.0 - position).clamp(0.0, 1.0);
        ((value - value * saturation * ramp) * 255.0).round() as u8
    };
    [channel(5.0), channel(3.0), channel(1.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(colors: &[([u8; 3], usize)]) -> Vec<u8> {
        colors
            .iter()
            .flat_map(|([red, green, blue], count)| {
                std::iter::repeat_n([*red, *green, *blue, 255], *count * SAMPLE_STEP)
            })
            .flatten()
            .collect()
    }

    #[test]
    fn the_color_of_a_cover_wins_over_its_black_borders() {
        let cover = pixels(&[([0, 0, 0], 800), ([200, 60, 30], 200)]);
        assert_eq!(dominant_color(&cover), Some([200, 60, 30]));
    }

    #[test]
    fn a_black_and_white_cover_stays_neutral() {
        let cover = pixels(&[([0, 0, 0], 500), ([250, 250, 250], 400)]);
        let (top, bottom) = page_tint(dominant_color(&cover).expect("pixels"));
        assert_eq!(top[0], top[1]);
        assert_eq!(top[1], top[2]);
        assert!(bottom[0] < top[0]);
    }

    #[test]
    fn the_tint_keeps_the_hue_and_stays_dark_enough_for_white_text() {
        let (top, bottom) = page_tint([220, 40, 20]);
        assert!(top[0] > top[1] && top[0] > top[2], "{top:?}");
        assert!(top.iter().all(|channel| *channel <= 110), "{top:?}");
        assert!(bottom.iter().all(|channel| *channel <= 40), "{bottom:?}");
    }

    #[test]
    fn no_pixels_gives_no_color() {
        assert_eq!(dominant_color(&[]), None);
    }
}
