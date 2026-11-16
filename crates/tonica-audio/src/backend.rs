//! Backend abstraction: the real (cpal) and the fake (virtual clock) backends
//! implement [`Backend`].

use crate::types::{AudioError, BackendEvent, DeviceInfo, OpenRequest, Stamp, StreamInfo};

/// Input callback: interleaved `f32` block and its stamp.
pub type InputFn = Box<dyn FnMut(&[f32], Stamp) + Send>;
/// Output callback: interleaved `f32` block to fill and its stamp.
pub type OutputFn = Box<dyn FnMut(&mut [f32], Stamp) + Send>;
/// Where an xrun was, in frames of the stream it belongs to (`XrunIn`: input, `XrunOut`:
/// output).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XrunBoundary {
    /// The frame counter of the stream at the instant the backend saw the error: the
    /// `Stamp::first` of the first block after the lost frames.
    Exact(u64),
    /// The backend saw the xrun but lost its exact boundary (its boundary queue was
    /// full): the boundary is somewhere in `from..=to`. The client must not guess a
    /// place inside that span: nothing in it is judged.
    Within {
        /// Earliest possible boundary.
        from: u64,
        /// Latest possible boundary.
        to: u64,
    },
}

/// Error callback: the event and, for an xrun, its boundary ([`XrunBoundary`]).
///
/// `None` when the backend does not know the boundary at all (and for every event that
/// is not an xrun). The callback may be delivered late, after the block at the boundary
/// has already run: the boundary, not the time of delivery, says where the xrun was.
pub type ErrorFn = Box<dyn FnMut(BackendEvent, Option<XrunBoundary>) + Send>;

/// Callbacks handed to a backend. They run on the real-time threads: no
/// allocation, no locks, no printing.
pub struct IoCallbacks {
    /// Input block, interleaved `f32`.
    pub input: InputFn,
    /// Output block to fill, interleaved `f32`.
    pub output: OutputFn,
    /// Stream errors and device events. May run on the RT thread: atomics only.
    pub error: ErrorFn,
}

/// The clock of the stamps, readable at any time.
pub trait HostClock {
    /// The host clock now, in ns: the clock `Stamp::device_ns` and `Stamp::callback_ns` of
    /// the output stream are on (input stamps are brought to it by the backend). A plain
    /// clock read: cheap, no lock held across a callback, callable from any thread that
    /// may own the streams.
    fn host_now_ns(&self) -> u64;
}

/// An audio backend. Dropping `Streams` stops and closes the streams.
pub trait Backend {
    /// Handle that owns the open streams.
    type Streams: HostClock;

    /// Lists the available devices.
    fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError>;

    /// Opens the input and output streams and starts them.
    fn open(
        &self,
        req: &OpenRequest,
        io: IoCallbacks,
    ) -> Result<(Self::Streams, StreamInfo), AudioError>;
}
