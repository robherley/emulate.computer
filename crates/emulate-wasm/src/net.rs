//! Logical relay connections supplied by the browser's MessagePort transport.

use emulate_core::hostnet::{ConnId, RelayClient, RelayTransport, TransportEvent};
use js_sys::{Array, Uint8Array};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[derive(Clone)]
    pub type BrowserTransport;
    #[wasm_bindgen(method, structural)]
    fn open(this: &BrowserTransport) -> u32;
    #[wasm_bindgen(method, structural, js_name = sendText)]
    fn send_text(this: &BrowserTransport, id: u32, text: &str) -> bool;
    #[wasm_bindgen(method, structural, js_name = sendBinary)]
    fn send_binary(this: &BrowserTransport, id: u32, data: &[u8]) -> bool;
    #[wasm_bindgen(method, structural, js_name = bufferedAmount)]
    fn buffered_amount(this: &BrowserTransport, id: u32) -> u32;
    #[wasm_bindgen(method, structural)]
    fn close(this: &BrowserTransport, id: u32);
    #[wasm_bindgen(method, structural)]
    fn poll(this: &BrowserTransport) -> Array;
    #[wasm_bindgen(method, structural)]
    fn dispose(this: &BrowserTransport);
}

pub struct PortTransport(BrowserTransport);
impl RelayTransport for PortTransport {
    fn open(&mut self, _: &str) -> Option<ConnId> {
        let id = self.0.open();
        (id != 0).then_some(ConnId(u64::from(id)))
    }
    fn send_text(&mut self, conn: ConnId, text: &str) -> bool {
        self.0.send_text(conn.0 as u32, text)
    }
    fn send_binary(&mut self, conn: ConnId, data: &[u8]) -> bool {
        self.0.send_binary(conn.0 as u32, data)
    }
    fn buffered_amount(&self, conn: ConnId) -> u64 {
        u64::from(self.0.buffered_amount(conn.0 as u32))
    }
    fn close(&mut self, conn: ConnId) {
        self.0.close(conn.0 as u32);
    }
    fn poll(&mut self, _: u64) -> Vec<TransportEvent> {
        self.0
            .poll()
            .iter()
            .filter_map(|event| {
                let event = Array::from(&event);
                let kind = event.get(0).as_f64()? as u32;
                let id = ConnId(event.get(1).as_f64()? as u64);
                match kind {
                    0 => Some(TransportEvent::Opened(id)),
                    1 => Some(TransportEvent::Text(id, event.get(2).as_string()?)),
                    2 => Some(TransportEvent::Binary(
                        id,
                        Uint8Array::new(&event.get(2)).to_vec(),
                    )),
                    3 => Some(TransportEvent::Closed(id)),
                    _ => Some(TransportEvent::Error(id)),
                }
            })
            .collect()
    }
}
impl Drop for PortTransport {
    fn drop(&mut self) {
        self.0.dispose();
    }
}
pub fn relay_substrate(url: &str, transport: BrowserTransport) -> RelayClient<PortTransport> {
    RelayClient::new(PortTransport(transport), url)
}
