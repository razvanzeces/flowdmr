//! UDP transport for FlowDMR IPC frames (sidecar -> FlowStation entity).

use std::net::{ToSocketAddrs, UdpSocket};

use flowdmr_ipc::FlowDmrFrame;

use crate::session::FrameSink;

/// Sends encoded frames to the FlowStation entity over UDP.
pub struct UdpSink {
    socket: UdpSocket,
}

impl UdpSink {
    /// Create a sink bound to an ephemeral local port, "connected" to `dest` so
    /// every `send` goes to the entity. Binds `0.0.0.0` (not loopback) so the OS
    /// picks the right outbound interface — this is what lets the decoder run on
    /// a SEPARATE machine and stream audio to FlowStation over the LAN (needed
    /// when the BTS's own TX desensitises a co-located RTL-SDR). Works for a
    /// localhost `dest` too: the loopback route is still chosen automatically.
    pub fn connect(dest: &str) -> std::io::Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        let addr = dest
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "entity addr did not resolve"))?;
        socket.connect(addr)?;
        Ok(Self { socket })
    }

    /// Clone the underlying socket so each worker thread owns its own handle.
    pub fn try_clone(&self) -> std::io::Result<Self> {
        Ok(Self { socket: self.socket.try_clone()? })
    }
}

impl FrameSink for UdpSink {
    fn send(&mut self, frame: FlowDmrFrame) {
        let bytes = frame.encode();
        if let Err(e) = self.socket.send(&bytes) {
            tracing::trace!("flowdmr-sidecar: IPC send failed: {e}");
        }
    }
}
