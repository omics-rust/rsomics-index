use std::io::{self, Cursor, Read, Write};
use std::num::NonZero;

use rsomics_index::bgzip::{CompressOptions, GziIndex, compress, decompress, decompress_indexed};
use rsomics_index::commands::bgzip::{Mode, RunOptions, run};

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
    let fixture = include_bytes!("golden/text.txt");
    let input = fixture.repeat(2_000);
    let (encoded, _) = compress(input.as_slice(), Vec::new(), &CompressOptions::default()).unwrap();
    let block_size = usize::from(u16::from_le_bytes([encoded[16], encoded[17]])) + 1;
    let mut reader = noodles_bgzf::io::Reader::new(&encoded[..block_size]);
    let mut first_block = Vec::new();
    reader.read_to_end(&mut first_block).unwrap();

    assert_eq!(first_block.last(), Some(&b'\n'));
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
fn bgzf_compression_surfaces_a_midstream_sink_failure() {
    #[derive(Debug)]
    struct WriteFailure {
        remaining: usize,
    }

    impl Write for WriteFailure {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "deliberate sink failure",
                ));
            }
            let size = self.remaining.min(buffer.len());
            self.remaining -= size;
            Ok(size)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let input = vec![b'A'; 2_000_000];
    let options = CompressOptions {
        workers: NonZero::new(4).unwrap(),
        ..CompressOptions::default()
    };

    let error = compress(input.as_slice(), WriteFailure { remaining: 100 }, &options).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(error.to_string(), "deliberate sink failure");
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

#[test]
fn bgzf_decompression_rejects_noncanonical_empty_blocks() {
    let (mut encoded, _) =
        compress(b"ACGT".as_slice(), Vec::new(), &CompressOptions::default()).unwrap();
    let mut empty = encoded[encoded.len() - 28..].to_vec();
    empty[9] = 0x03;
    let eof_start = encoded.len() - 28;
    encoded.splice(eof_start..eof_start, empty);

    let error = decompress(encoded.as_slice(), Vec::new(), None).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn gzi_partial_read_matches_the_requested_uncompressed_range() {
    let input = (0..300_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let options = CompressOptions {
        text: false,
        ..CompressOptions::default()
    };
    let (encoded, _) = compress(input.as_slice(), Vec::new(), &options).unwrap();
    let index = GziIndex::scan(&mut Cursor::new(&encoded)).unwrap();
    let mut output = Vec::new();

    decompress_indexed(
        Cursor::new(encoded),
        &index,
        65_000,
        Some(131_000),
        &mut output,
    )
    .unwrap();

    assert_eq!(output, input[65_000..196_000]);
}

#[test]
fn indexed_decompression_without_size_requires_the_eof_marker() {
    let input = (0..300_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let options = CompressOptions {
        text: false,
        ..CompressOptions::default()
    };
    let (mut encoded, _) = compress(input.as_slice(), Vec::new(), &options).unwrap();
    let index = GziIndex::scan(&mut Cursor::new(&encoded)).unwrap();
    encoded.truncate(encoded.len() - 28);

    let error =
        decompress_indexed(Cursor::new(encoded), &index, 65_000, None, Vec::new()).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn gzi_rejects_compressed_offsets_outside_bgzf_virtual_positions() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(&(1u64 << 48).to_le_bytes());
    bytes.extend_from_slice(&1u64.to_le_bytes());

    let error = GziIndex::read(bytes.as_slice()).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn gzi_scan_rejects_blocks_larger_than_the_bgzf_uncompressed_limit() {
    let input = vec![b'A'; 100_000];
    let (mut encoded, _) =
        compress(input.as_slice(), Vec::new(), &CompressOptions::default()).unwrap();
    let first_block_size = usize::from(u16::from_le_bytes([encoded[16], encoded[17]])) + 1;
    encoded[first_block_size - 4..first_block_size].copy_from_slice(&65_537u32.to_le_bytes());

    let error = GziIndex::scan(&mut Cursor::new(encoded)).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn gzi_serialization_round_trips_scanned_block_offsets() {
    let input = (0..300_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let options = CompressOptions {
        text: false,
        ..CompressOptions::default()
    };
    let (encoded, _) = compress(input.as_slice(), Vec::new(), &options).unwrap();
    let expected = GziIndex::scan(&mut Cursor::new(encoded)).unwrap();
    let mut bytes = Vec::new();
    expected.write(&mut bytes).unwrap();

    let actual = GziIndex::read(bytes.as_slice()).unwrap();

    assert_eq!(actual, expected);
    for &(compressed, uncompressed) in actual.entries() {
        let position = actual.query(uncompressed).unwrap();
        assert_eq!(position.compressed(), compressed);
        assert_eq!(position.uncompressed(), 0);
    }
}

#[test]
fn gzi_rejects_nonincreasing_offsets_and_trailing_bytes() {
    let mut nonincreasing = Vec::new();
    nonincreasing.extend_from_slice(&2u64.to_le_bytes());
    nonincreasing.extend_from_slice(&100u64.to_le_bytes());
    nonincreasing.extend_from_slice(&100u64.to_le_bytes());
    nonincreasing.extend_from_slice(&99u64.to_le_bytes());
    nonincreasing.extend_from_slice(&200u64.to_le_bytes());
    let error = GziIndex::read(nonincreasing.as_slice()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    let mut trailing = 0u64.to_le_bytes().to_vec();
    trailing.push(0);
    let error = GziIndex::read(trailing.as_slice()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn gzi_scan_rejects_a_truncated_eof_marker() {
    let (mut encoded, _) =
        compress(b"ACGT".as_slice(), Vec::new(), &CompressOptions::default()).unwrap();
    encoded.pop();

    let error = GziIndex::scan(&mut Cursor::new(encoded)).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn failed_named_output_preserves_destination() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("truncated.bgz");
    let output = directory.path().join("output.txt");
    let (mut encoded, _) = compress(
        b"new data".as_slice(),
        Vec::new(),
        &CompressOptions::default(),
    )
    .unwrap();
    encoded.pop();
    std::fs::write(&input, encoded).unwrap();
    std::fs::write(&output, b"old data").unwrap();
    let options = RunOptions {
        mode: Mode::Decompress,
        input,
        output: Some(output.clone()),
        force: true,
        ..RunOptions::default()
    };

    let error = run(&options).unwrap_err();

    assert!(error.to_string().contains("EOF"), "{error}");
    assert_eq!(std::fs::read(output).unwrap(), b"old data");
}

#[test]
fn named_compression_commits_a_matching_gzi_pair() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.bin");
    let output = directory.path().join("output.bgz");
    let index = directory.path().join("output.bgz.gzi");
    let partial = directory.path().join("partial.bin");
    let data = (0..300_000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    std::fs::write(&input, &data).unwrap();
    let compression = CompressOptions {
        text: false,
        ..CompressOptions::default()
    };

    let summary = run(&RunOptions {
        input: input.clone(),
        output: Some(output.clone()),
        index_output: Some(index.clone()),
        compression,
        ..RunOptions::default()
    })
    .unwrap();

    assert!(summary.index_entries.is_some_and(|count| count > 0));
    let loaded = GziIndex::read(std::fs::File::open(&index).unwrap()).unwrap();
    assert_eq!(
        loaded.entries().len() as u64,
        summary.index_entries.unwrap()
    );

    run(&RunOptions {
        mode: Mode::Decompress,
        input: output,
        output: Some(partial.clone()),
        index_input: Some(index),
        offset: Some(65_000),
        size: Some(131_000),
        ..RunOptions::default()
    })
    .unwrap();

    assert_eq!(std::fs::read(partial).unwrap(), data[65_000..196_000]);
}

#[test]
fn named_output_requires_force_before_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.txt");
    let output = directory.path().join("output.bgz");
    std::fs::write(&input, b"new data").unwrap();
    std::fs::write(&output, b"old data").unwrap();

    let error = run(&RunOptions {
        input,
        output: Some(output.clone()),
        ..RunOptions::default()
    })
    .unwrap_err();

    assert!(error.to_string().contains("--force"), "{error}");
    assert_eq!(std::fs::read(output).unwrap(), b"old data");
}

#[test]
fn gzi_rejects_entries_that_cannot_be_adjacent_bgzf_blocks() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(&65_537u64.to_le_bytes());
    bytes.extend_from_slice(&65_537u64.to_le_bytes());

    let error = GziIndex::read(bytes.as_slice()).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn reindex_rejects_crc_corruption_and_preserves_existing_index() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("corrupt.bgz");
    let index = directory.path().join("corrupt.bgz.gzi");
    let (mut encoded, _) = compress(
        b"ACGT".repeat(40_000).as_slice(),
        Vec::new(),
        &CompressOptions::default(),
    )
    .unwrap();
    let first_block_size = usize::from(u16::from_le_bytes([encoded[16], encoded[17]])) + 1;
    encoded[first_block_size - 8] ^= 0xff;
    std::fs::write(&input, encoded).unwrap();
    std::fs::write(&index, b"old index").unwrap();

    let error = run(&RunOptions {
        mode: Mode::Reindex,
        input,
        index_output: Some(index.clone()),
        force: true,
        ..RunOptions::default()
    })
    .unwrap_err();

    assert!(error.to_string().contains("checksum"), "{error}");
    assert_eq!(std::fs::read(index).unwrap(), b"old index");
}
