//! Wire framing: u32 big-endian length followed by 1..=65536 payload bytes.
use rad_plugin_sdk::frame::{self, FrameError, MAX_PAYLOAD};
use std::io::Cursor;

#[test]
fn encodes_a_big_endian_length_header_followed_by_the_payload() {
    assert_eq!(
        frame::encode(b"{}").unwrap(),
        [0, 0, 0, 2, b'{', b'}'],
        "length 2 as four big-endian bytes"
    );
    let big = vec![b'x'; 0x0102];
    let encoded = frame::encode(&big).unwrap();
    assert_eq!(encoded[..4], [0, 0, 0x01, 0x02]);
    assert_eq!(encoded[4..], big[..]);
}

#[test]
fn reads_consecutive_frames_then_reports_a_clean_end_of_stream() {
    let mut stream = Vec::new();
    stream.extend_from_slice(&[0, 0, 0, 3]);
    stream.extend_from_slice(b"abc");
    stream.extend_from_slice(&[0, 0, 0, 1]);
    stream.extend_from_slice(b"z");
    let mut input = Cursor::new(stream);
    assert_eq!(frame::read(&mut input).unwrap(), Some(b"abc".to_vec()));
    assert_eq!(frame::read(&mut input).unwrap(), Some(b"z".to_vec()));
    assert_eq!(frame::read(&mut input).unwrap(), None);
}

#[test]
fn accepts_the_limits_and_rejects_everything_outside_them() {
    // Largest legal payload round-trips.
    let largest = vec![b'a'; MAX_PAYLOAD];
    let encoded = frame::encode(&largest).unwrap();
    assert_eq!(
        frame::read(&mut Cursor::new(encoded)).unwrap().unwrap(),
        largest
    );
    // Sizes outside 1..=65536 are refused when writing...
    assert!(matches!(frame::encode(b""), Err(FrameError::Empty)));
    assert!(matches!(
        frame::encode(&vec![0; MAX_PAYLOAD + 1]),
        Err(FrameError::TooLarge(n)) if n == MAX_PAYLOAD + 1
    ));
    // ...and when reading, judged from the header alone (no payload follows).
    assert!(matches!(
        frame::read(&mut Cursor::new([0, 0, 0, 0])),
        Err(FrameError::Empty)
    ));
    assert!(matches!(
        frame::read(&mut Cursor::new([0, 1, 0, 1])),
        Err(FrameError::TooLarge(65537))
    ));
    assert!(matches!(
        frame::read(&mut Cursor::new([0xff, 0xff, 0xff, 0xff])),
        Err(FrameError::TooLarge(_))
    ));
}

#[test]
fn a_stream_cut_inside_a_frame_is_an_error_not_a_clean_end() {
    assert!(matches!(
        frame::read(&mut Cursor::new([0, 0])),
        Err(FrameError::Truncated)
    ));
    assert!(matches!(
        frame::read(&mut Cursor::new([0, 0, 0, 5, b'a', b'b'])),
        Err(FrameError::Truncated)
    ));
}

#[test]
fn write_emits_exactly_the_encoded_frame() {
    let mut out = Vec::new();
    frame::write(&mut out, b"hello").unwrap();
    assert_eq!(out, frame::encode(b"hello").unwrap());
    assert!(matches!(
        frame::write(&mut out, b""),
        Err(FrameError::Empty)
    ));
    assert_eq!(out.len(), 9, "a refused frame writes nothing");
}
