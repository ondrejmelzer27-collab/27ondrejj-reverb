//! Lock-free audio → GUI sample transport for the spectrum analyzer.
//!
//! A single-producer/single-consumer [`rtrb`] ring buffer, allocated once when the
//! plugin is constructed. The audio thread pushes mono `(L + R) / 2` samples of the
//! processed output; when the ring is full (GUI closed or slow) the excess is dropped.
//! The audio side never blocks, allocates, or locks.
//!
//! The consumer lives behind a `Mutex` only so the receiver can be cloned into each
//! editor instance; that mutex is touched exclusively by GUI threads.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use rtrb::{Consumer, Producer, RingBuffer};

/// Ring capacity in mono samples (~0.68 s at 48 kHz, ~0.34 s at 96 kHz).
pub const ANALYZER_CAPACITY: usize = 1 << 15;

struct Shared {
    /// Current sample rate as `f32` bits.
    sample_rate_bits: AtomicU32,
}

/// Audio-thread end. Owned by the plugin struct; used from `process()`.
pub struct AnalyzerProducer {
    producer: Producer<f32>,
    shared: Arc<Shared>,
}

/// GUI end. Cheap to clone; every clone reads from the same ring.
#[derive(Clone)]
pub struct AnalyzerReceiver {
    consumer: Arc<Mutex<Consumer<f32>>>,
    shared: Arc<Shared>,
}

/// Create the bus. Allocates the ring once; call outside the audio thread.
pub fn analyzer_bus() -> (AnalyzerProducer, AnalyzerReceiver) {
    let (producer, consumer) = RingBuffer::<f32>::new(ANALYZER_CAPACITY);
    let shared = Arc::new(Shared {
        sample_rate_bits: AtomicU32::new(48_000.0f32.to_bits()),
    });
    (
        AnalyzerProducer {
            producer,
            shared: shared.clone(),
        },
        AnalyzerReceiver {
            consumer: Arc::new(Mutex::new(consumer)),
            shared,
        },
    )
}

impl AnalyzerProducer {
    /// Publish the current sample rate (call from `activate()`).
    pub fn set_sample_rate(&self, sample_rate: f32) {
        self.shared
            .sample_rate_bits
            .store(sample_rate.to_bits(), Ordering::Relaxed);
    }

    /// Push the mono sum of a stereo block. Realtime-safe: no allocation, no locks.
    /// Writes as many frames as fit and drops the rest. Returns frames written.
    pub fn push_stereo(&mut self, left: &[f32], right: &[f32]) -> usize {
        let n = left.len().min(right.len()).min(self.producer.slots());
        if n == 0 {
            return 0;
        }
        match self.producer.write_chunk_uninit(n) {
            Ok(chunk) => chunk.fill_from_iter(
                left[..n]
                    .iter()
                    .zip(&right[..n])
                    .map(|(l, r)| 0.5 * (l + r)),
            ),
            Err(_) => 0,
        }
    }
}

impl AnalyzerReceiver {
    /// Sample rate of the samples in the ring.
    pub fn sample_rate(&self) -> f32 {
        f32::from_bits(self.shared.sample_rate_bits.load(Ordering::Relaxed))
    }

    /// GUI thread: drain every available sample, oldest first. `f` is called with up to
    /// two contiguous slices (the ring may wrap). Returns the number of samples drained.
    pub fn drain(&self, mut f: impl FnMut(&[f32])) -> usize {
        let Ok(mut consumer) = self.consumer.lock() else {
            return 0;
        };
        let n = consumer.slots();
        if n == 0 {
            return 0;
        }
        match consumer.read_chunk(n) {
            Ok(chunk) => {
                let (a, b) = chunk.as_slices();
                if !a.is_empty() {
                    f(a);
                }
                if !b.is_empty() {
                    f(b);
                }
                chunk.commit_all();
                n
            }
            Err(_) => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_drain_roundtrip() {
        let (mut tx, rx) = analyzer_bus();
        let l = [1.0f32, 2.0, 3.0];
        let r = [3.0f32, 2.0, 1.0];
        assert_eq!(tx.push_stereo(&l, &r), 3);
        let mut got = Vec::new();
        assert_eq!(rx.drain(|s| got.extend_from_slice(s)), 3);
        assert_eq!(got, vec![2.0, 2.0, 2.0]);
        assert_eq!(rx.drain(|_| panic!("should be empty")), 0);
    }

    #[test]
    fn overflow_drops_excess() {
        let (mut tx, rx) = analyzer_bus();
        let block = vec![0.25f32; ANALYZER_CAPACITY + 100];
        assert_eq!(tx.push_stereo(&block, &block), ANALYZER_CAPACITY);
        assert_eq!(tx.push_stereo(&block, &block), 0);
        assert_eq!(rx.drain(|_| {}), ANALYZER_CAPACITY);
    }

    #[test]
    fn sample_rate_is_shared() {
        let (tx, rx) = analyzer_bus();
        tx.set_sample_rate(96_000.0);
        assert_eq!(rx.sample_rate(), 96_000.0);
    }
}
