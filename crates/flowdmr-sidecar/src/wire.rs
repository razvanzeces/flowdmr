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
    /// every `send` goes to the entity. Localhost only in practice.
    pub fn connect(dest: &str) -> std::io::Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
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
