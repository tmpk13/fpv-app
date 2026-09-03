// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Block reassembly: fragments in, the original packets out, in order.
//!
//! An air unit cuts the video stream into blocks of `k` packets, adds `n - k`
//! parity fragments, and sends all `n`. This end collects them and hands back
//! the first `k`, recovering any that went missing.
//!
//! Two things about the ordering are worth stating, because they are what
//! make the difference between a decoder that works and one that stutters:
//!
//! - A fragment is released the moment every fragment before it has been
//!   released. Waiting for the block to complete would add a whole block of
//!   latency to a link that is usually lossless, and on FPV video that is the
//!   wrong trade every time.
//! - FEC is applied only when a gap actually blocks progress. A block that
//!   arrives complete costs no arithmetic at all, which is what keeps this
//!   affordable on a phone.
//!
//! Blocks are held in a small ring. When a block older than the ring's span
//! is still incomplete it is flushed with the gaps left in - late is worse
//! than lossy here, and the alternative is unbounded delay.

use super::fec::Fec;

/// How many blocks may be in flight at once.
///
/// The radio reorders a little and the FEC means a block is not done until
/// its last useful fragment lands, so some depth is needed; past that, depth
/// is just latency waiting to happen.
const RING_SIZE: usize = 40;

/// The largest payload a fragment can carry after decryption.
///
/// wfb-ng's 4045-byte injected frame less the 802.11 header, the block header
/// and the authentication tag.
pub const MAX_FEC_PAYLOAD: usize = 4045 - 24 - 9 - 16;

/// The largest packet a fragment can hold, once its own 3-byte header is out.
pub const MAX_PAYLOAD: usize = MAX_FEC_PAYLOAD - 3;

/// Set on a fragment that exists only to carry parity and holds no packet.
const FLAG_FEC_ONLY: u8 = 0x01;

/// One block of the stream, filling up.
struct Block {
    block_idx: u64,
    /// `n` slots; `Some` once the fragment has arrived or been recovered.
    fragments: Vec<Option<Vec<u8>>>,
    have: usize,
    /// How far into the block the caller has already been given packets.
    sent: usize,
}

impl Block {
    fn new(n: usize) -> Self {
        Self {
            block_idx: 0,
            fragments: (0..n).map(|_| None).collect(),
            have: 0,
            sent: 0,
        }
    }

    fn reset(&mut self, block_idx: u64) {
        self.block_idx = block_idx;
        self.fragments.iter_mut().for_each(|f| *f = None);
        self.have = 0;
        self.sent = 0;
    }
}

/// What reassembly saw, for the Link page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AggStats {
    /// Packets handed on to the video path.
    pub packets_out: u64,
    pub bytes_out: u64,
    /// Packets that never arrived and could not be recovered, counted from
    /// the gaps in the sequence the blocks imply.
    pub packets_lost: u64,
    /// Packets the erasure code rebuilt. This is the number that says how
    /// hard the link is working: loss the user never sees.
    pub recovered: u64,
    /// Blocks flushed incomplete because newer ones had overtaken them.
    pub overrun: u64,
    /// Fragments whose own header did not describe something inside them.
    pub corrupt: u64,
}

/// The block ring for one session key.
///
/// Rebuilt whenever the air unit announces a new session, because `k` and `n`
/// can change with it and every block in flight belongs to the old code.
pub struct Aggregator {
    fec: Fec,
    ring: Vec<Block>,
    front: usize,
    alloc: usize,
    /// Highest block index seen. `None` until the first fragment, which is
    /// what stops the first block ever looking like a huge forward jump.
    last_known_block: Option<u64>,
    /// Sequence number of the last packet released, for the loss count.
    seq: Option<u64>,
    stats: AggStats,
}

impl Aggregator {
    pub fn new(fec: Fec) -> Self {
        let n = fec.n();
        Self {
            fec,
            ring: (0..RING_SIZE).map(|_| Block::new(n)).collect(),
            front: 0,
            alloc: 0,
            last_known_block: None,
            seq: None,
            stats: AggStats::default(),
        }
    }

    pub fn stats(&self) -> AggStats {
        self.stats
    }

    /// Adopt the counts of the aggregator this one replaces.
    ///
    /// A session rekey builds a new block ring, but the link on the other side
    /// of it did not restart. Without this the Link page would zero itself
    /// every time the air unit rotated its key, which is the one moment a user
    /// most wants the history.
    pub fn restore(&mut self, stats: AggStats) {
        self.stats = stats;
    }

    pub fn k(&self) -> usize {
        self.fec.k()
    }

    pub fn n(&self) -> usize {
        self.fec.n()
    }

    /// Take one decrypted fragment and release whatever packets it completes.
    ///
    /// `out` is called once per packet, in stream order.
    pub fn push(
        &mut self,
        block_idx: u64,
        fragment_idx: usize,
        data: Vec<u8>,
        out: &mut dyn FnMut(&[u8]),
    ) {
        if fragment_idx >= self.fec.n() {
            self.stats.corrupt += 1;
            return;
        }

        let Some(slot) = self.locate(block_idx, out) else {
            // A block already finished and dropped out of the ring. This is
            // normal: a late duplicate, or a fragment that lost a race with
            // the FEC that already rebuilt it.
            return;
        };

        if self.ring[slot].fragments[fragment_idx].is_some() {
            return;
        }
        self.ring[slot].fragments[fragment_idx] = Some(data);
        self.ring[slot].have += 1;

        let k = self.fec.k();

        // The oldest block can release packets as soon as they are contiguous
        // from where it left off, without waiting for the block to fill.
        if slot == self.front {
            while self.ring[slot].sent < k
                && self.ring[slot].fragments[self.ring[slot].sent].is_some()
            {
                self.emit(slot, self.ring[slot].sent, out);
                self.ring[slot].sent += 1;
            }
            if self.ring[slot].sent == k {
                self.pop_front();
                return;
            }
        }

        // `k` fragments with gaps among the first `k` is exactly the point at
        // which the erasure code can finish the block.
        if self.ring[slot].sent < k && self.ring[slot].have == k {
            // Anything still open in front of this block will never complete
            // now; flush what it has and move on.
            let ahead = (slot + RING_SIZE - self.front) % RING_SIZE;
            for _ in 0..ahead {
                self.flush_front(out);
                self.pop_front();
            }

            self.recover(slot);

            while self.ring[slot].sent < k {
                if self.ring[slot].fragments[self.ring[slot].sent].is_some() {
                    self.emit(slot, self.ring[slot].sent, out);
                }
                self.ring[slot].sent += 1;
            }
            self.pop_front();
        }
    }

    /// The ring slot for `block_idx`, allocating slots for any blocks skipped
    /// on the way. `None` when the block has already been dealt with.
    fn locate(&mut self, block_idx: u64, out: &mut dyn FnMut(&[u8])) -> Option<usize> {
        for i in 0..self.alloc {
            let slot = (self.front + i) % RING_SIZE;
            if self.ring[slot].block_idx == block_idx {
                return Some(slot);
            }
        }

        match self.last_known_block {
            Some(last) if block_idx <= last => return None,
            _ => {}
        }

        // Every block between the last one seen and this one is now known to
        // exist, and gets a slot so its fragments still have somewhere to land
        // if they turn up late.
        let new_blocks = match self.last_known_block {
            Some(last) => (block_idx - last).min(RING_SIZE as u64) as usize,
            None => 1,
        };
        self.last_known_block = Some(block_idx);

        let mut slot = 0;
        for i in 0..new_blocks {
            slot = self.push_slot(out);
            self.ring[slot].reset(block_idx + 1 + i as u64 - new_blocks as u64);
        }
        Some(slot)
    }

    /// Claim a slot at the back of the ring, evicting the front if it is full.
    fn push_slot(&mut self, out: &mut dyn FnMut(&[u8])) -> usize {
        if self.alloc < RING_SIZE {
            let slot = (self.front + self.alloc) % RING_SIZE;
            self.alloc += 1;
            return slot;
        }

        // The ring is full of unfinished blocks, so the oldest has waited as
        // long as it usefully can.
        self.stats.overrun += 1;
        self.flush_front(out);
        let slot = self.front;
        self.front = (self.front + 1) % RING_SIZE;
        slot
    }

    /// Release whatever the front block has, gaps and all.
    fn flush_front(&mut self, out: &mut dyn FnMut(&[u8])) {
        let slot = self.front;
        for idx in self.ring[slot].sent..self.fec.k() {
            if self.ring[slot].fragments[idx].is_some() {
                self.emit(slot, idx, out);
            }
        }
    }

    fn pop_front(&mut self) {
        self.front = (self.front + 1) % RING_SIZE;
        self.alloc = self.alloc.saturating_sub(1);
    }

    /// Rebuild the missing data fragments of a block that has `k` in total.
    fn recover(&mut self, slot: usize) {
        let k = self.fec.k();
        let n = self.fec.n();

        // The erasure code wants exactly `k` fragments, each labelled with
        // its own number, and every present data fragment at its own
        // position. Parity fragments fill the holes, in the order they come.
        let mut index = Vec::with_capacity(k);
        let mut have: Vec<&[u8]> = Vec::with_capacity(k);
        let mut spare = k;
        let mut size = 0;

        for i in 0..k {
            match &self.ring[slot].fragments[i] {
                Some(frag) => {
                    index.push(i);
                    have.push(frag);
                }
                None => {
                    while spare < n && self.ring[slot].fragments[spare].is_none() {
                        spare += 1;
                    }
                    let Some(frag) = self.ring[slot]
                        .fragments
                        .get(spare)
                        .and_then(|f| f.as_ref())
                    else {
                        // Cannot happen with `have == k`, and if it somehow
                        // did, the block simply keeps its gaps.
                        return;
                    };
                    // Parity fragments are as long as the longest packet in
                    // the block, so they are what sets the recovery width.
                    size = size.max(frag.len());
                    index.push(spare);
                    have.push(frag);
                    spare += 1;
                }
            }
        }

        let recovered = match self.fec.decode(&have, &index, size) {
            Ok(blocks) => blocks,
            Err(err) => {
                log::debug!("wfb: cannot recover block: {err}");
                return;
            }
        };

        let missing: Vec<usize> = (0..k)
            .filter(|i| self.ring[slot].fragments[*i].is_none())
            .collect();
        for (i, data) in missing.into_iter().zip(recovered) {
            self.ring[slot].fragments[i] = Some(data);
            self.ring[slot].have += 1;
            self.stats.recovered += 1;
        }
    }

    /// Hand one fragment's packet to the caller, and account for any packets
    /// the stream skipped to get here.
    fn emit(&mut self, slot: usize, fragment_idx: usize, out: &mut dyn FnMut(&[u8])) {
        let k = self.fec.k() as u64;
        let seq = self.ring[slot].block_idx * k + fragment_idx as u64;
        if let Some(last) = self.seq {
            if seq > last + 1 {
                self.stats.packets_lost += seq - last - 1;
            }
        }
        self.seq = Some(seq);

        let Some(frag) = self.ring[slot].fragments[fragment_idx].as_deref() else {
            return;
        };
        let Some(header) = frag.get(..3) else {
            self.stats.corrupt += 1;
            return;
        };
        let flags = header[0];
        let size = usize::from(u16::from_be_bytes([header[1], header[2]]));

        if size > MAX_PAYLOAD || 3 + size > frag.len() {
            // A length its own fragment cannot hold. On a received fragment
            // this is a corrupt sender; on a recovered one it means the
            // recovery was fed a fragment that was not what it claimed.
            self.stats.corrupt += 1;
            return;
        }
        if flags & FLAG_FEC_ONLY != 0 {
            // A padding fragment, present only to give the block its shape.
            return;
        }

        self.stats.packets_out += 1;
        self.stats.bytes_out += size as u64;
        out(&frag[3..3 + size]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrap a payload the way a sender does: flags, big-endian length, data.
    fn fragment(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8];
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn fec_only(len: usize) -> Vec<u8> {
        let mut out = vec![FLAG_FEC_ONLY];
        out.extend_from_slice(&(len as u16).to_be_bytes());
        out.extend_from_slice(&vec![0u8; len]);
        out
    }

    /// A block of `k` payloads plus its parity, as the air unit would send.
    fn encode_block(fec: &Fec, payloads: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let frags: Vec<Vec<u8>> = payloads.iter().map(|p| fragment(p)).collect();
        let size = frags.iter().map(|f| f.len()).max().unwrap();
        let refs: Vec<&[u8]> = frags.iter().map(|f| f.as_slice()).collect();
        let mut all = frags.clone();
        all.extend(fec.encode(&refs, size));
        all
    }

    fn payloads(k: usize, base: u8) -> Vec<Vec<u8>> {
        (0..k)
            .map(|i| vec![base.wrapping_add(i as u8); 20 + i * 3])
            .collect()
    }

    #[test]
    fn a_clean_block_comes_out_whole_and_in_order() {
        let mut agg = Aggregator::new(Fec::new(4, 6).unwrap());
        let want = payloads(4, 10);
        let frags = encode_block(&Fec::new(4, 6).unwrap(), &want);

        let mut got: Vec<Vec<u8>> = Vec::new();
        for (i, f) in frags.iter().enumerate() {
            agg.push(0, i, f.clone(), &mut |p| got.push(p.to_vec()));
        }
        assert_eq!(got, want);
        assert_eq!(agg.stats().recovered, 0);
        assert_eq!(agg.stats().packets_out, 4);
    }

    #[test]
    fn a_packet_is_released_before_its_block_finishes() {
        let mut agg = Aggregator::new(Fec::new(4, 6).unwrap());
        let want = payloads(4, 1);
        let frags = encode_block(&Fec::new(4, 6).unwrap(), &want);

        let mut got: Vec<Vec<u8>> = Vec::new();
        agg.push(0, 0, frags[0].clone(), &mut |p| got.push(p.to_vec()));
        assert_eq!(
            got,
            vec![want[0].clone()],
            "the first fragment of the oldest block must not wait for the rest"
        );
    }

    #[test]
    fn a_lost_fragment_is_rebuilt_from_parity() {
        let fec = Fec::new(8, 12).unwrap();
        let mut agg = Aggregator::new(Fec::new(8, 12).unwrap());
        let want = payloads(8, 40);
        let frags = encode_block(&fec, &want);

        let mut got: Vec<Vec<u8>> = Vec::new();
        for (i, f) in frags.iter().enumerate() {
            // Drop data fragments 2 and 5; the parity makes up the count.
            if i == 2 || i == 5 {
                continue;
            }
            agg.push(7, i, f.clone(), &mut |p| got.push(p.to_vec()));
        }
        assert_eq!(got, want);
        assert_eq!(agg.stats().recovered, 2);
        assert_eq!(agg.stats().packets_lost, 0, "recovered loss is not loss");
    }

    #[test]
    fn fragments_arriving_out_of_order_still_come_out_in_order() {
        let fec = Fec::new(4, 6).unwrap();
        let mut agg = Aggregator::new(Fec::new(4, 6).unwrap());
        let want = payloads(4, 60);
        let frags = encode_block(&fec, &want);

        let mut got: Vec<Vec<u8>> = Vec::new();
        for i in [3usize, 1, 0, 2] {
            agg.push(11, i, frags[i].clone(), &mut |p| got.push(p.to_vec()));
        }
        assert_eq!(got, want);
    }

    #[test]
    fn a_duplicate_fragment_is_not_delivered_twice() {
        let fec = Fec::new(2, 4).unwrap();
        let mut agg = Aggregator::new(Fec::new(2, 4).unwrap());
        let want = payloads(2, 3);
        let frags = encode_block(&fec, &want);

        let mut got: Vec<Vec<u8>> = Vec::new();
        agg.push(0, 0, frags[0].clone(), &mut |p| got.push(p.to_vec()));
        agg.push(0, 0, frags[0].clone(), &mut |p| got.push(p.to_vec()));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn an_unrecoverable_block_costs_only_its_own_packets() {
        let fec = Fec::new(4, 6).unwrap();
        let mut agg = Aggregator::new(Fec::new(4, 6).unwrap());
        let first = payloads(4, 1);
        let second = payloads(4, 100);
        let a = encode_block(&fec, &first);
        let b = encode_block(&fec, &second);

        let mut got: Vec<Vec<u8>> = Vec::new();
        // Block 0 loses three of six fragments, which is one too many.
        for i in [0usize, 4, 5] {
            agg.push(0, i, a[i].clone(), &mut |p| got.push(p.to_vec()));
        }
        // Block 1 arrives whole, which forces block 0 out of the ring.
        for (i, f) in b.iter().enumerate() {
            agg.push(1, i, f.clone(), &mut |p| got.push(p.to_vec()));
        }

        let mut want = vec![first[0].clone()];
        want.extend(second.clone());
        assert_eq!(got, want);
        assert_eq!(
            agg.stats().packets_lost,
            3,
            "three packets of block 0 are gone and the count must say so"
        );
    }

    #[test]
    fn a_block_stalled_past_the_ring_is_flushed_rather_than_held() {
        let fec = Fec::new(2, 4).unwrap();
        let mut agg = Aggregator::new(Fec::new(2, 4).unwrap());
        let mut got: Vec<Vec<u8>> = Vec::new();

        // Every block keeps only its second fragment, so none can ever
        // release anything in order and none reaches k. That is the only way
        // the ring actually fills: a block that completes flushes the ones
        // before it on its own.
        let mut second = Vec::new();
        for block in 0..=(RING_SIZE as u64) {
            let want = payloads(2, block as u8);
            let frags = encode_block(&fec, &want);
            second.push(want[1].clone());
            agg.push(block, 1, frags[1].clone(), &mut |p| got.push(p.to_vec()));
        }

        assert!(
            agg.stats().overrun > 0,
            "a ring full of stalled blocks must evict the oldest"
        );
        assert_eq!(
            got,
            vec![second[0].clone()],
            "an evicted block still gives up the fragments it did have"
        );
    }

    #[test]
    fn a_completed_block_flushes_the_stalled_ones_before_it() {
        let fec = Fec::new(2, 4).unwrap();
        let mut agg = Aggregator::new(Fec::new(2, 4).unwrap());
        let mut got: Vec<Vec<u8>> = Vec::new();
        let mut want: Vec<Vec<u8>> = Vec::new();

        let whole = |agg: &mut Aggregator, got: &mut Vec<Vec<u8>>, block, base| {
            let payloads = payloads(2, base);
            for (i, f) in encode_block(&fec, &payloads).iter().enumerate() {
                agg.push(block, i, f.clone(), &mut |p| got.push(p.to_vec()));
            }
            payloads
        };

        want.extend(whole(&mut agg, &mut got, 0, 10));

        // Block 1 can never finish: it holds fragment 1 and nothing else.
        let stalled = payloads(2, 20);
        let stuck = encode_block(&fec, &stalled);
        agg.push(1, 1, stuck[1].clone(), &mut |p| got.push(p.to_vec()));
        assert_eq!(got.len(), 2, "fragment 1 cannot precede fragment 0");

        // Block 2 arrives whole, and block 1 is now past saving. Its one
        // fragment goes out before block 2's, so the stream stays in order.
        want.push(stalled[1].clone());
        want.extend(whole(&mut agg, &mut got, 2, 30));

        assert_eq!(got, want);
        assert_eq!(
            agg.stats().packets_lost,
            1,
            "the first packet of block 1 never arrived and cannot be rebuilt"
        );
    }

    #[test]
    fn a_padding_fragment_carries_no_packet() {
        let fec = Fec::new(3, 5).unwrap();
        let mut agg = Aggregator::new(Fec::new(3, 5).unwrap());
        // The last fragment of a short block is padding, which is how a
        // sender fills a block out when the video does not.
        let frags = vec![fragment(&[1, 2, 3]), fragment(&[4, 5, 6]), fec_only(6)];
        let size = frags.iter().map(|f| f.len()).max().unwrap();
        let refs: Vec<&[u8]> = frags.iter().map(|f| f.as_slice()).collect();
        let mut all = frags.clone();
        all.extend(fec.encode(&refs, size));

        let mut got: Vec<Vec<u8>> = Vec::new();
        for (i, f) in all.iter().enumerate() {
            agg.push(0, i, f.clone(), &mut |p| got.push(p.to_vec()));
        }
        assert_eq!(got, vec![vec![1, 2, 3], vec![4, 5, 6]]);
    }

    #[test]
    fn a_fragment_claiming_more_than_it_holds_is_dropped() {
        let mut agg = Aggregator::new(Fec::new(1, 2).unwrap());
        let mut got: Vec<Vec<u8>> = Vec::new();
        // Header says 500 bytes, fragment holds four.
        agg.push(0, 0, vec![0x00, 0x01, 0xf4, 0xaa], &mut |p| {
            got.push(p.to_vec())
        });
        assert!(got.is_empty());
        assert_eq!(agg.stats().corrupt, 1);
    }

    #[test]
    fn a_fragment_index_outside_the_code_is_dropped() {
        let mut agg = Aggregator::new(Fec::new(2, 4).unwrap());
        let mut got: Vec<Vec<u8>> = Vec::new();
        agg.push(0, 4, fragment(&[1, 2, 3]), &mut |p| got.push(p.to_vec()));
        assert!(got.is_empty());
        assert_eq!(agg.stats().corrupt, 1);
    }

    #[test]
    fn a_replayed_old_block_is_ignored() {
        let fec = Fec::new(2, 4).unwrap();
        let mut agg = Aggregator::new(Fec::new(2, 4).unwrap());
        let mut got: Vec<Vec<u8>> = Vec::new();

        for block in 0..3u64 {
            let frags = encode_block(&fec, &payloads(2, block as u8));
            for (i, f) in frags.iter().enumerate() {
                agg.push(block, i, f.clone(), &mut |p| got.push(p.to_vec()));
            }
        }
        let count = got.len();

        let old = encode_block(&fec, &payloads(2, 0));
        agg.push(0, 0, old[0].clone(), &mut |p| got.push(p.to_vec()));
        assert_eq!(got.len(), count, "block 0 was finished three blocks ago");
    }

    #[test]
    fn a_long_gap_in_blocks_does_not_allocate_the_world() {
        let fec = Fec::new(2, 4).unwrap();
        let mut agg = Aggregator::new(Fec::new(2, 4).unwrap());
        let mut got: Vec<Vec<u8>> = Vec::new();

        let frags = encode_block(&fec, &payloads(2, 1));
        for (i, f) in frags.iter().enumerate() {
            agg.push(0, i, f.clone(), &mut |p| got.push(p.to_vec()));
        }
        // A very distant block, as a restarted air unit would produce.
        for (i, f) in frags.iter().enumerate() {
            agg.push(1_000_000, i, f.clone(), &mut |p| got.push(p.to_vec()));
        }
        assert_eq!(got.len(), 4);
        assert!(agg.alloc <= RING_SIZE);
    }
}
