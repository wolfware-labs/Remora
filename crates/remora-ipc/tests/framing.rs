#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::io::Cursor;

use remora_ipc::{FrameError, MAX_MESSAGE_BYTES, read_frame, write_frame};

#[test]
fn messages_are_read_one_line_at_a_time() {
    let mut input = Cursor::new(b"{\"a\":1}\n{\"b\":2}\n".to_vec());

    assert_eq!(read_frame(&mut input).unwrap().as_deref(), Some(r#"{"a":1}"#));
    assert_eq!(read_frame(&mut input).unwrap().as_deref(), Some(r#"{"b":2}"#));
    assert_eq!(read_frame(&mut input).unwrap(), None, "a clean end of the stream");
}

#[test]
fn a_message_of_exactly_the_maximum_size_is_accepted() {
    let mut input = Cursor::new([vec![b'x'; MAX_MESSAGE_BYTES], vec![b'\n']].concat());
    assert_eq!(read_frame(&mut input).unwrap().unwrap().len(), MAX_MESSAGE_BYTES);
}

#[test]
fn an_oversized_message_is_refused_without_reading_it_all() {
    let mut input = Cursor::new([vec![b'x'; MAX_MESSAGE_BYTES + 1], vec![b'\n']].concat());

    assert!(matches!(read_frame(&mut input), Err(FrameError::TooLarge)));
    assert_eq!(input.position(), MAX_MESSAGE_BYTES as u64 + 1);
}

#[test]
fn a_message_cut_off_by_the_end_of_the_stream_is_truncated() {
    let mut input = Cursor::new(b"{\"half\":".to_vec());
    assert!(matches!(read_frame(&mut input), Err(FrameError::Truncated)));
}

#[test]
fn a_message_that_is_not_utf8_is_refused() {
    let mut input = Cursor::new(b"\xff\xfe\n".to_vec());
    assert!(matches!(read_frame(&mut input), Err(FrameError::NotUtf8)));
}

#[test]
fn written_messages_are_single_lines() {
    let mut out = Vec::new();
    write_frame(&mut out, &serde_json::json!({"text": "two\nlines"})).unwrap();
    assert_eq!(out, b"{\"text\":\"two\\nlines\"}\n");
}

#[test]
fn an_oversized_message_is_never_written() {
    let mut out = Vec::new();
    let huge = "x".repeat(MAX_MESSAGE_BYTES);
    assert!(matches!(write_frame(&mut out, &huge), Err(FrameError::TooLarge)));
    assert!(out.is_empty());
}
