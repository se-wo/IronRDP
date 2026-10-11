//! Shared fixture: a real IronRDP EGFX server wired to a real IronRDP EGFX
//! client, exchanging exactly the bytes they would put on the DVC.
//!
//! Server -> client messages are the ZGFX-wrapped PDUs from
//! `GraphicsPipelineServer::drain_output`; client -> server messages are the
//! plain PDUs the client returns from `DvcProcessor::process`.

#![allow(dead_code)]

use ironrdp_dvc::{DvcMessage, DvcProcessor as _};
use ironrdp_egfx::client::GraphicsPipelineClient;
use ironrdp_egfx::decode::OpenH264Decoder;
use ironrdp_egfx::pdu::{CapabilitiesAdvertisePdu, CapabilitySet};
use ironrdp_egfx::server::{GraphicsPipelineHandler as ServerHandler, GraphicsPipelineServer};

const CHANNEL: u32 = 7;

struct NoopServerHandler;

impl ServerHandler for NoopServerHandler {
    fn capabilities_advertise(&mut self, _pdu: &CapabilitiesAdvertisePdu) {}
    fn on_ready(&mut self, _negotiated: &CapabilitySet) {}
}

struct NoopClientHandler;

impl ironrdp_egfx::client::GraphicsPipelineHandler for NoopClientHandler {}

pub struct EgfxPair {
    pub server: GraphicsPipelineServer,
    pub client: GraphicsPipelineClient,
    /// Client-side mirror of the output buffer, rebuilt from
    /// `GraphicsPipelineClient::drain_output` (the compositor's final state).
    pub output: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

fn bytes(msgs: Vec<DvcMessage>) -> Vec<Vec<u8>> {
    msgs.iter()
        .map(|m| ironrdp_core::encode_vec(m.as_ref()).expect("encode DVC message"))
        .collect()
}

impl EgfxPair {
    /// Negotiate (client default caps = V8.1 with AVC420, OpenH264 decoder),
    /// create one `width` x `height` surface and map it to output (0, 0).
    pub fn new(width: u16, height: u16) -> Self {
        let mut server = GraphicsPipelineServer::new(Box::new(NoopServerHandler));
        let mut client = GraphicsPipelineClient::new(
            Box::new(NoopClientHandler),
            Some(Box::new(OpenH264Decoder::new().expect("openh264 decoder"))),
        );
        server.start(CHANNEL).unwrap();
        let advertise = bytes(client.start(CHANNEL).unwrap());
        let mut pair = Self {
            server,
            client,
            output: vec![0; usize::from(width) * usize::from(height) * 4],
            width: usize::from(width),
            height: usize::from(height),
        };
        for msg in advertise {
            let replies = pair.server.process(CHANNEL, &msg).unwrap();
            pair.deliver(bytes(replies));
        }
        assert!(pair.server.supports_avc420(), "AVC420 must be negotiated");
        let surface = pair.server.create_surface(width, height).expect("surface");
        assert!(pair.server.map_surface_to_output(surface, 0, 0));
        pair.pump();
        pair
    }

    /// Deliver server output to the client, feed client replies (frame acks)
    /// back to the server, and apply the compositor's output updates.
    pub fn pump(&mut self) {
        let out = bytes(self.server.drain_output());
        self.deliver(out);
    }

    fn deliver(&mut self, server_msgs: Vec<Vec<u8>>) {
        for msg in server_msgs {
            let replies = self.client.process(CHANNEL, &msg).expect("client process");
            for reply in bytes(replies) {
                let _ = self.server.process(CHANNEL, &reply).expect("server process");
            }
        }
        for update in self.client.drain_output() {
            let r = &update.region;
            let w = usize::from(r.right - r.left);
            for (row, y) in (usize::from(r.top)..usize::from(r.bottom)).enumerate() {
                let dst = (y * self.width + usize::from(r.left)) * 4;
                self.output[dst..dst + w * 4].copy_from_slice(&update.data[row * w * 4..(row + 1) * w * 4]);
            }
        }
    }

    pub fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.width + x) * 4;
        [self.output[i], self.output[i + 1], self.output[i + 2]]
    }

    /// Mean of (R+G+B)/3 over a rectangle of the output mirror.
    pub fn mean_level(&self, left: usize, top: usize, right: usize, bottom: usize) -> f64 {
        let mut sum = 0u64;
        for y in top..bottom {
            for x in left..right {
                let [r, g, b] = self.pixel(x, y);
                sum += u64::from(r) + u64::from(g) + u64::from(b);
            }
        }
        sum as f64 / (3.0 * ((right - left) * (bottom - top)) as f64)
    }
}
