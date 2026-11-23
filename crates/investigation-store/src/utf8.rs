//! UTF-8 validation of a byte stream in fixed-size chunks.
//!
//! Text decoding checks the retained bytes without holding them in memory:
//! peak use is one buffer of [`BUFFER_BYTES`] whatever the input size. The
//! answer is the same as `std::str::from_utf8` over the whole input.
use std::io::{self, ErrorKind, Read};

/// Size of the one buffer the check reads into.
pub const BUFFER_BYTES: usize = 64 * 1024;

/// Whether everything `input` yields is valid UTF-8.
pub fn is_valid(mut input: impl Read) -> io::Result<bool> {
    let mut buffer = vec![0u8; BUFFER_BYTES];
    // Bytes of a sequence that the previous chunk cut short, moved to the
    // start of the buffer. A UTF-8 sequence is at most four bytes, so this is
    // at most three.
    let mut carried = 0;
    loop {
        let read = match input.read(&mut buffer[carried..]) {
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if read == 0 {
            return Ok(carried == 0);
        }
        let filled = carried + read;
        carried = match std::str::from_utf8(&buffer[..filled]) {
            Ok(_) => 0,
            // An invalid sequence, not merely one the chunk boundary cut.
            Err(error) if error.error_len().is_some() => return Ok(false),
            Err(error) => {
                let valid = error.valid_up_to();
                buffer.copy_within(valid..filled, 0);
                filled - valid
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Yields its input in pieces of a fixed size, so sequences straddle reads.
    struct Pieces<'a>(&'a [u8], usize);

    impl Read for Pieces<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let take = self.0.len().min(self.1).min(buffer.len());
            buffer[..take].copy_from_slice(&self.0[..take]);
            self.0 = &self.0[take..];
            Ok(take)
        }
    }

    #[test]
    fn agrees_with_the_whole_input_check_across_every_split() {
        let cases: &[&[u8]] = &[
            b"",
            b"plain ascii",
            "café ☕ 𝄞".as_bytes(),
            b"\xff",
            b"ok\xc3",           // truncated two-byte sequence at the end
            b"\xe2\x82",         // truncated three-byte sequence
            b"\xf0\x9d\x84",     // truncated four-byte sequence
            b"\xc0\xaf",         // overlong `/`
            b"\xed\xa0\x80",     // UTF-16 surrogate half
            b"\xf4\x90\x80\x80", // above U+10FFFF
            b"a\x80b",           // lone continuation byte
        ];
        for bytes in cases {
            for piece in 1..=5 {
                assert_eq!(
                    is_valid(Pieces(bytes, piece)).unwrap(),
                    std::str::from_utf8(bytes).is_ok(),
                    "{bytes:?} in pieces of {piece}"
                );
            }
        }
    }

    #[test]
    fn a_sequence_split_at_the_buffer_boundary_is_carried() {
        let mut bytes = vec![b'a'; BUFFER_BYTES - 1];
        bytes.extend("𝄞".as_bytes());
        bytes.extend(b"tail");
        assert!(is_valid(bytes.as_slice()).unwrap());
        bytes.truncate(BUFFER_BYTES + 1);
        assert!(!is_valid(bytes.as_slice()).unwrap());
    }
}
