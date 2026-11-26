//! The spool's own tests: the disk segment round-trips in order, and
//! its file goes away when the drain empties it.

use super::spool::Spool;

#[test]
fn frames_round_trip_across_blocks_in_order() {
    let mut spool = Spool::create("test").expect("the spool creates");
    // Two frames straddling several blocks, distinguishable at
    // the seams.
    let first = vec![0xA5u8; Spool::BLOCK + 1024];
    let second = vec![0x5Au8; 333];
    spool.push(&first).expect("push");
    spool.push(&second).expect("push");
    assert_eq!(spool.pending_bytes(), (first.len() + second.len()) as u64);

    // Drain until empty; the block count matches the ceiling of
    // bytes / block, and the bytes come back in order.
    let mut drained: Vec<u8> = Vec::new();
    let mut steps = 0;
    while !spool.is_empty() {
        let block = spool.take_block().expect("the read");
        drained.extend_from_slice(&block);
        steps += 1;
    }
    let expected_blocks = (first.len() + second.len()).div_ceil(Spool::BLOCK);
    assert_eq!(steps, expected_blocks);
    assert_eq!(drained.len(), first.len() + second.len());
    // In order: the seam is the first frame's tail then the second.
    assert_eq!(&drained[..first.len()], &first[..]);
    assert_eq!(&drained[first.len()..], &second[..]);
    assert!(spool.is_empty());
}

#[test]
fn the_drained_spool_is_empty_and_its_file_goes_away() {
    let mut spool = Spool::create("test").expect("the spool creates");
    spool.push(b"frame").expect("push");
    let path = spool.path().to_path_buf();
    let block = spool.take_block().expect("the read");
    assert_eq!(block, b"frame");
    assert!(spool.is_empty());
    drop(spool);
    assert!(!path.exists(), "dropping a drained spool removes its file");
}
