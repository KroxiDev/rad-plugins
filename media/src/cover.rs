//! Cover art: the picture a player offers, made to fit a button.
//!
//! A player names its cover with a URL it chooses. Nothing is downloaded: only a file on this
//! machine is read, with its size and the size of the picture bounded before anything is decoded.
use image::{
    DynamicImage, ExtendedColorType, ImageEncoder, ImageError, ImageReader, Limits, Rgb, RgbImage,
    codecs::jpeg::JpegEncoder, imageops::FilterType,
};
use std::{
    fs::OpenOptions,
    io::{Cursor, Read},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        mpsc::{self, SyncSender},
    },
};

/// Side of the square picture the plugin publishes, in pixels.
pub const SIZE: u32 = 96;
/// Largest cover file read, in bytes.
const MAX_FILE: u64 = 8 * 1024 * 1024;
/// Largest width and height decoded. Both bound the memory a decode can take.
const MAX_SIDE: u32 = 2048;
/// Most memory a decoder may ask for. A picture of `MAX_SIDE` squared in 16 bits per channel needs
/// about 32 MiB, which fits; anything that would need more is refused.
const MAX_ALLOC: u64 = 64 * 1024 * 1024;
/// Heaviest JPEG the host accepts (`jpeg_hex` is at most 16384 characters).
const MAX_JPEG: usize = 8 * 1024;
/// What shows around a picture that is not square, and behind a transparent one.
const BACKGROUND: [u8; 3] = [0, 0, 0];
/// Qualities tried from best to worst until the JPEG fits.
const QUALITIES: [u8; 9] = [90, 80, 70, 60, 50, 40, 30, 20, 10];

/// Why a cover is not shown.
#[derive(Debug, PartialEq, Eq)]
pub enum CoverError {
    /// Not a `file://` URL on this machine: nothing is downloaded.
    NotLocal,
    /// The file cannot be opened or read, or is not a regular file.
    Unreadable,
    /// The file or the picture is bigger than the limits allow.
    TooBig,
    /// Not a PNG or JPEG picture.
    NotAPicture,
}

impl From<ImageError> for CoverError {
    fn from(error: ImageError) -> Self {
        match error {
            ImageError::Limits(_) => Self::TooBig,
            _ => Self::NotAPicture,
        }
    }
}

/// The file a `file://` cover URL names. A URL naming another host is refused.
pub fn path_of(art_url: &str) -> Result<PathBuf, CoverError> {
    match glib::filename_from_uri(art_url) {
        Ok((path, None)) if art_url.starts_with("file://") => Ok(path),
        _ => Err(CoverError::NotLocal),
    }
}

/// The bytes of a regular file of at most [`MAX_FILE`] bytes.
fn read_file(path: &Path) -> Result<Vec<u8>, CoverError> {
    // Not blocking, so that a named pipe cannot hold the reader: it is refused below instead.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
        .map_err(|_| CoverError::Unreadable)?;
    let metadata = file.metadata().map_err(|_| CoverError::Unreadable)?;
    if !metadata.is_file() {
        return Err(CoverError::Unreadable);
    }
    if metadata.len() > MAX_FILE {
        return Err(CoverError::TooBig);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    // The file may grow after its size was read: the limit applies to what is actually read.
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CoverError::Unreadable)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(CoverError::TooBig);
    }
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> Result<DynamicImage, CoverError> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| CoverError::NotAPicture)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    Ok(reader.decode()?)
}

/// The picture scaled to fit inside `SIZE` x `SIZE`, in the middle of a background of that size.
fn fitted(picture: &DynamicImage) -> RgbImage {
    let small = picture.resize(SIZE, SIZE, FilterType::Triangle).to_rgba8();
    let mut canvas = RgbImage::from_pixel(SIZE, SIZE, Rgb(BACKGROUND));
    let (left, top) = ((SIZE - small.width()) / 2, (SIZE - small.height()) / 2);
    for (x, y, pixel) in small.enumerate_pixels() {
        let alpha = u32::from(pixel[3]);
        let behind = canvas.get_pixel_mut(left + x, top + y);
        for channel in 0..3 {
            let over = u32::from(pixel[channel]) * alpha;
            let under = u32::from(behind[channel]) * (255 - alpha);
            behind[channel] = ((over + under + 127) / 255) as u8;
        }
    }
    canvas
}

/// The best JPEG of `canvas` that weighs at most [`MAX_JPEG`] bytes.
fn small_jpeg(canvas: &RgbImage) -> Result<Vec<u8>, CoverError> {
    for quality in QUALITIES {
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, quality)
            .write_image(canvas.as_raw(), SIZE, SIZE, ExtendedColorType::Rgb8)
            .map_err(|_| CoverError::NotAPicture)?;
        if bytes.len() <= MAX_JPEG {
            return Ok(bytes);
        }
    }
    Err(CoverError::TooBig)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    text
}

/// The cover in `path` as a `SIZE` x `SIZE` JPEG in lowercase hex, small enough to publish. The
/// kind of picture is taken from its content, not from the name of the file.
pub fn jpeg_hex(path: &Path) -> Result<String, CoverError> {
    convert(&read_file(path)?)
}

/// [`path_of`] and [`jpeg_hex`] in one.
pub fn jpeg_hex_from_url(art_url: &str) -> Result<String, CoverError> {
    jpeg_hex(&path_of(art_url)?)
}

fn convert(bytes: &[u8]) -> Result<String, CoverError> {
    let picture = decode(bytes)?;
    Ok(hex(&small_jpeg(&fitted(&picture))?))
}

type Conversion = (Vec<u8>, SyncSender<Result<String, CoverError>>);

/// One CPU converter for the current readers. Keeping decoder scratch on one thread avoids
/// retaining it in every application's allocator arena. File I/O stays on each app's reader:
/// a blocked filesystem cannot hold this shared worker. The rendezvous queues no extra work;
/// callers retain at most one bounded input each, as with concurrent decoding before.
#[derive(Default)]
pub(crate) struct Converter {
    sender: OnceLock<Option<SyncSender<Conversion>>>,
}

impl Converter {
    #[cfg(test)]
    pub(crate) fn stalled() -> (Self, mpsc::Receiver<Conversion>) {
        let (sender, receiver) = mpsc::sync_channel(0);
        (
            Self {
                sender: OnceLock::from(Some(sender)),
            },
            receiver,
        )
    }

    pub(crate) fn jpeg_hex(&self, path: &Path) -> Result<String, CoverError> {
        let bytes = read_file(path)?;
        let sender = self
            .sender
            .get_or_init(|| {
                let (sender, receiver) = mpsc::sync_channel::<Conversion>(0);
                std::thread::Builder::new()
                    .name("media-cover".into())
                    .spawn(move || {
                        while let Ok((bytes, reply)) = receiver.recv() {
                            let _ = reply.send(convert(&bytes));
                        }
                    })
                    .ok()
                    .map(|_| sender)
            })
            .as_ref()
            .ok_or(CoverError::Unreadable)?;
        let (reply, result) = mpsc::sync_channel(1);
        sender
            .send((bytes, reply))
            .map_err(|_| CoverError::Unreadable)?;
        result.recv().map_err(|_| CoverError::Unreadable)?
    }
}
