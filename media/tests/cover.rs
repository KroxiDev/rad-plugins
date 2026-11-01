//! Cover art: any local picture a player offers becomes a small JPEG that fits the button.
mod support;
use image::{
    ExtendedColorType, ImageEncoder, Rgb, RgbImage, Rgba, RgbaImage,
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
};
use rad_plugin_media::cover::{CoverError, SIZE, jpeg_hex, jpeg_hex_from_url, path_of};
use std::{
    fs,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// The most a JPEG may weigh for the host: 16384 hex characters.
const MAX_BYTES: usize = 8192;

fn dir() -> support::Scratch {
    support::scratch_dir("cover", false)
}

fn png(image: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .unwrap();
    out
}

fn jpeg(image: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 90)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}

fn solid(width: u32, height: u32, colour: [u8; 4]) -> RgbaImage {
    RgbaImage::from_pixel(width, height, Rgba(colour))
}

/// Deterministic noise: the worst case for a JPEG.
fn noise(width: u32, height: u32) -> RgbaImage {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    RgbaImage::from_fn(width, height, |_, _| {
        let mut byte = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        };
        Rgba([byte(), byte(), byte(), 255])
    })
}

/// Decodes the hex a cover produced, checking it is a JPEG of the right size and weight.
fn decoded(hex: &str) -> RgbImage {
    assert!(
        hex.len().is_multiple_of(2) && hex.len() <= 2 * MAX_BYTES,
        "{} hex characters",
        hex.len()
    );
    assert!(
        hex.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    assert_eq!(&bytes[..3], [0xff, 0xd8, 0xff], "a JPEG starts so");
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).unwrap();
    assert_eq!((image.width(), image.height()), (SIZE, SIZE));
    image.to_rgb8()
}

fn cover_of(bytes: &[u8]) -> Result<String, CoverError> {
    let dir = dir();
    let file = dir.join("cover");
    fs::write(&file, bytes).unwrap();
    jpeg_hex(&file)
}

fn near(pixel: &Rgb<u8>, expected: [u8; 3]) -> bool {
    pixel
        .0
        .iter()
        .zip(expected)
        .all(|(a, b)| a.abs_diff(b) <= 24)
}

#[test]
fn a_png_cover_becomes_a_small_square_jpeg() {
    let hex = cover_of(&png(&solid(256, 256, [200, 30, 30, 255]))).unwrap();
    let image = decoded(&hex);
    assert!(near(image.get_pixel(48, 48), [200, 30, 30]));
    assert!(
        near(image.get_pixel(2, 2), [200, 30, 30]),
        "a square fills the button"
    );
}

#[test]
fn a_jpeg_cover_is_recognised_by_content_not_by_file_name() {
    // Players often store covers with no extension, or the wrong one.
    let bytes = jpeg(&RgbImage::from_pixel(300, 300, Rgb([20, 160, 40])));
    let dir = dir();
    for name in ["cover", "cover.png", "cover.jpg"] {
        let file = dir.join(name);
        fs::write(&file, &bytes).unwrap();
        let image = decoded(&jpeg_hex(&file).unwrap());
        assert!(near(image.get_pixel(48, 48), [20, 160, 40]), "{name}");
    }
}

#[test]
fn a_busy_picture_still_fits_in_the_size_the_host_allows() {
    // Noise does not compress: the quality is lowered until it fits. At its own size the picture
    // keeps all its noise (a larger one is averaged away when it is scaled down).
    decoded(&cover_of(&png(&noise(96, 96))).unwrap());
    decoded(&cover_of(&png(&noise(512, 512))).unwrap());
    decoded(
        &cover_of(&jpeg(&RgbImage::from_fn(640, 480, |x, y| {
            Rgb([(x * 7) as u8, (y * 5) as u8, (x ^ y) as u8])
        })))
        .unwrap(),
    );
}

#[test]
fn the_largest_accepted_picture_is_accepted_and_one_pixel_more_is_not() {
    let wide = |w: u32, h: u32| cover_of(&png(&solid(w, h, [10, 10, 200, 255])));
    decoded(&wide(2048, 2048).unwrap());
    assert_eq!(wide(2049, 8), Err(CoverError::TooBig));
    assert_eq!(wide(8, 2049), Err(CoverError::TooBig));
}

#[test]
fn a_picture_with_a_huge_size_is_refused_quickly_without_decoding_it() {
    // 4000 x 4000 grey pixels are a few KiB in the file and 16 MB once decoded.
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&vec![0u8; 4000 * 4000], 4000, 4000, ExtendedColorType::L8)
        .unwrap();
    assert!(bytes.len() < 200_000, "{} bytes", bytes.len());
    let started = Instant::now();
    assert_eq!(cover_of(&bytes), Err(CoverError::TooBig));
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_file_over_eight_mebibytes_is_refused_unread() {
    let dir = dir();
    let file = dir.join("big");
    let handle = fs::File::create(&file).unwrap();
    handle.set_len(8 * 1024 * 1024 + 1).unwrap(); // sparse: nothing is written
    assert_eq!(jpeg_hex(&file), Err(CoverError::TooBig));
    handle.set_len(8 * 1024 * 1024).unwrap();
    assert_eq!(
        jpeg_hex(&file),
        Err(CoverError::NotAPicture),
        "at the limit it is read, and is not a picture"
    );
}

#[test]
fn what_is_not_a_png_or_jpeg_picture_is_refused() {
    for bytes in [
        b"".as_slice(),
        b"just text",
        b"GIF89a\x01\x00\x01\x00\x00\x00\x00;",
        b"\x89PNG\r\n\x1a\n truncated",
        b"\xff\xd8\xff truncated",
    ] {
        assert_eq!(cover_of(bytes), Err(CoverError::NotAPicture), "{bytes:?}");
    }
}

#[test]
fn files_that_are_not_regular_files_are_never_read() {
    let dir = dir();
    assert_eq!(jpeg_hex(&dir.join("missing")), Err(CoverError::Unreadable));
    assert_eq!(jpeg_hex(&dir), Err(CoverError::Unreadable), "a directory");
    assert_eq!(
        jpeg_hex(Path::new("/dev/zero")),
        Err(CoverError::Unreadable),
        "a device"
    );
    // A named pipe nobody writes to would block a plain open or read for ever.
    let fifo = dir.join("pipe");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `name` is a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let (sender, receiver) = std::sync::mpsc::channel();
    let path = fifo.clone();
    std::thread::spawn(move || sender.send(jpeg_hex(&path)));
    let answer = receiver.recv_timeout(Duration::from_secs(2));
    if answer.is_err() {
        // Let the stuck reader go, so that nothing is left hanging.
        let _ = fs::OpenOptions::new().write(true).open(&fifo);
    }
    assert_eq!(
        answer,
        Ok(Err(CoverError::Unreadable)),
        "a named pipe held the reader"
    );
}

#[test]
fn only_a_file_url_on_this_machine_names_a_cover() {
    assert_eq!(
        path_of("file:///music/cover.jpg"),
        Ok(PathBuf::from("/music/cover.jpg"))
    );
    assert_eq!(
        path_of("file:///music/a%20b/%C3%A9.png"),
        Ok(PathBuf::from("/music/a b/é.png")),
        "percent-encoding is undone"
    );
    for refused in [
        "https://example.invalid/cover.jpg",
        "http://example.invalid/cover.jpg",
        "file://example.invalid/cover.jpg",
        "data:image/png;base64,AAAA",
        "/music/cover.jpg",
        "cover.jpg",
        "",
        "file:",
        "FTP://example.invalid/cover.jpg",
    ] {
        assert_eq!(path_of(refused), Err(CoverError::NotLocal), "{refused}");
    }
}

#[test]
fn a_url_leads_to_the_cover_in_its_file() {
    let dir = dir();
    let file = dir.join("with space.png");
    fs::write(&file, png(&solid(32, 32, [0, 0, 255, 255]))).unwrap();
    let url = format!("file://{}", dir.join("with%20space.png").display());
    let image = decoded(&jpeg_hex_from_url(&url).unwrap());
    assert!(near(image.get_pixel(48, 48), [0, 0, 255]));
    assert_eq!(
        jpeg_hex_from_url("https://example.invalid/a.png"),
        Err(CoverError::NotLocal)
    );
}
