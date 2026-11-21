use png::{BitDepth, ColorType, Compression, Encoder};

pub(crate) fn png_hex(percent: u32) -> Option<String> {
    const WIDTH: usize = 96;
    const HEIGHT: usize = 24;
    const TRACK: [u8; 3] = [64, 69, 80];
    let fill: [u8; 3] = match percent {
        0..60 => [65, 205, 132],
        60..85 => [244, 182, 68],
        _ => [242, 94, 90],
    };
    let fill_end = 8.0 + 80.0 * percent as f32 / 100.0;
    let mut pixels = [0; WIDTH * HEIGHT * 4];
    for y in 5..19 {
        for x in 8..88 {
            let cx = x as f32 + 0.5;
            let cy = y as f32 + 0.5;
            // Distance to the centre segment of the 80 × 14 capsule, sampled at pixel centres.
            // `sqrt` rather than `hypot`: the same pixels at this scale, without linking libm.
            let (dx, dy) = (cx - cx.clamp(15.0, 81.0), cy - 12.0);
            let distance = (dx * dx + dy * dy).sqrt();
            let coverage = (7.5 - distance).clamp(0.0, 1.0);
            let filled = (fill_end - x as f32).clamp(0.0, 1.0);
            let offset = (y * WIDTH + x) * 4;
            for channel in 0..3 {
                let track = f32::from(TRACK[channel]);
                pixels[offset + channel] =
                    (track + filled * (f32::from(fill[channel]) - track)).round() as u8;
            }
            pixels[offset + 3] = (coverage * 255.0).round() as u8;
        }
    }

    let mut bytes = Vec::new();
    let mut encoder = Encoder::new(&mut bytes, WIDTH as u32, HEIGHT as u32);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    encoder.set_compression(Compression::Fast);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&pixels).ok()?;
    writer.finish().ok()?;

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Some(hex)
}

#[cfg(test)]
mod tests {
    use crate::{
        polling::Sample,
        presentation::{Presentation, Source},
    };
    use png::{BitDepth, ColorType, Decoder};
    use std::io::Cursor;

    fn image(percent: u32) -> Vec<u8> {
        let parameters = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "values": {"style": "bar"}
        }))
        .unwrap();
        let content = Presentation::parse(Source::Cpu, &parameters)
            .unwrap()
            .content(
                Source::Cpu,
                &Sample {
                    cpu: Some(percent),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(content.text, Some(format!("CPU {percent}%")));
        assert!(content.is_valid());
        let hex = content.png_hex.expect("usage bar includes a PNG");
        assert!(hex.len() <= 16384);
        let png: Vec<_> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let mut reader = Decoder::new(Cursor::new(png)).read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(
            (info.width, info.height, info.color_type, info.bit_depth),
            (96, 24, ColorType::Rgba, BitDepth::Eight)
        );
        pixels
    }
    fn pixel(pixels: &[u8], x: usize, y: usize) -> [u8; 4] {
        let index = (y * 96 + x) * 4;
        pixels[index..index + 4].try_into().unwrap()
    }

    #[test]
    fn proportional_fill_has_rounded_track_and_transparent_margins() {
        let empty = image(0);
        let track = pixel(&empty, 48, 12);
        assert_eq!(track[3], 255);
        for percent in [0, 25, 50, 75, 100] {
            let pixels = image(percent);
            let width = (8..88)
                .filter(|&x| pixel(&pixels, x, 12) != pixel(&empty, x, 12))
                .count();
            assert_eq!(width, 80 * percent as usize / 100);
            for y in 0..24 {
                for x in 0..96 {
                    if !(8..88).contains(&x) || !(5..19).contains(&y) {
                        assert_eq!(pixel(&pixels, x, y)[3], 0);
                    }
                }
            }
            assert_eq!(pixel(&pixels, 8, 5)[3], 0);
            assert!(
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|rgba| (1..255).contains(&rgba[3]))
            );
        }
    }

    #[test]
    fn fill_turns_amber_at_sixty_and_red_at_eighty_five() {
        let fill = |percent| pixel(&image(percent), 20, 12);
        let green = fill(59);
        let amber = fill(60);
        let red = fill(85);
        assert!(green[1] > green[0] && green[1] > green[2]);
        assert!(amber[0] > amber[1] && amber[1] > amber[2]);
        assert_eq!(fill(84), amber);
        assert!(red[0] > red[1] && red[1] < amber[1]);
        assert_eq!(fill(100), red);
    }
}
