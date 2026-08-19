use std::io::{self, Read, Write};
use std::num::NonZero;

use rsomics_index::bgzip::{CompressOptions, compress, decompress};

#[test]
fn bgzf_round_trip_preserves_multiblock_input() {
    let input = (0..200_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let options = CompressOptions {
        level: 6,
        workers: NonZero::new(2).unwrap(),
        text: false,
    };

    let (encoded, stats) = compress(input.as_slice(), Vec::new(), &options).unwrap();
    let mut decoded = Vec::new();
    decompress(encoded.as_slice(), &mut decoded, None).unwrap();

    assert_eq!(decoded, input);
    assert!(stats.blocks >= 4);
    assert_eq!(stats.bytes_in, 200_000);
    assert_eq!(stats.bytes_out, encoded.len() as u64);
}

#[test]
fn bgzf_decompression_rejects_a_truncated_eof_marker() {
    let input = vec![b'A'; 200_000];
    let (mut encoded, _) =
        compress(input.as_slice(), Vec::new(), &CompressOptions::default()).unwrap();
    encoded.pop();

    let error = decompress(encoded.as_slice(), Vec::new(), None).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn bgzf_text_mode_ends_full_blocks_at_a_newline() {
    let line = format!("{}\n", "ACGT".repeat(25));
    let input = line.repeat(1_000).into_bytes();
    let (encoded, _) = compress(input.as_slice(), Vec::new(), &CompressOptions::default()).unwrap();
    let block_size = usize::from(u16::from_le_bytes([encoded[16], encoded[17]])) + 1;
    let mut reader = noodles_bgzf::io::Reader::new(&encoded[..block_size]);
    let mut first_block = Vec::new();
    reader.read_to_end(&mut first_block).unwrap();

    assert_eq!(first_block.last(), Some(&b'\n'));
    assert_eq!(first_block.len() % line.len(), 0);
}

#[test]
fn bgzf_level_zero_writes_valid_stored_blocks() {
    let input = (0..100_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let options = CompressOptions {
        level: 0,
        text: false,
        ..CompressOptions::default()
    };
    let (encoded, _) = compress(input.as_slice(), Vec::new(), &options).unwrap();
    let mut decoded = Vec::new();
    decompress(encoded.as_slice(), &mut decoded, None).unwrap();

    assert_eq!(decoded, input);
}

#[test]
fn bgzf_rejects_compression_levels_above_nine_before_writing() {
    let options = CompressOptions {
        level: 10,
        ..CompressOptions::default()
    };

    let error = compress(b"ACGT".as_slice(), Vec::new(), &options).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn bgzf_compression_surfaces_a_final_sink_flush_failure() {
    #[derive(Debug)]
    struct FlushFailure;

    impl Write for FlushFailure {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

    let error = compress(
        b"ACGT".as_slice(),
        FlushFailure,
        &CompressOptions::default(),
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(error.to_string(), "flush failed");
}

#[test]
fn bgzf_decompression_rejects_duplicate_eof_markers() {
    let (mut encoded, _) =
        compress(b"ACGT".as_slice(), Vec::new(), &CompressOptions::default()).unwrap();
    let eof = encoded[encoded.len() - 28..].to_vec();
    encoded.extend_from_slice(&eof);

    let error = decompress(encoded.as_slice(), Vec::new(), None).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}
