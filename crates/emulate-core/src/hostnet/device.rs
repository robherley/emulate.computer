//! A `smoltcp::phy::Device` that is just two queues of IPv4 packets.
//!
//! Runs at `Medium::Ip`: Ethernet framing, ARP, DHCP, ICMP and DNS are handled
//! by this crate, so smoltcp only sees the IPv4 packets of TCP flows and stays
//! out of the ARP business (no neighbour cache, no proxy-ARP surprises from
//! `set_any_ip(true)`).

use std::collections::VecDeque;

use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;

use crate::hostnet::consts::MTU;

/// Cap on packets parked in `tx`. At the cap `transmit()` yields no token, so
/// smoltcp keeps data in its socket buffers rather than emitting segments we
/// would have to drop: dropping post-emission forces RTO retransmit stalls
/// (seen as truncated large downloads). The RxToken-paired token stays ungated
/// so inbound processing can always ACK; that overshoot is bounded by the rx
/// queue length.
pub(crate) const TX_QUEUE_LIMIT: usize = 128;

pub(crate) struct QueueDevice {
    /// Guest -> smoltcp (IPv4 packets, no Ethernet header).
    pub(crate) rx: VecDeque<Vec<u8>>,
    /// smoltcp -> guest (IPv4 packets, no Ethernet header).
    pub(crate) tx: VecDeque<Vec<u8>>,
}

impl QueueDevice {
    pub(crate) fn new() -> Self {
        Self {
            rx: VecDeque::new(),
            tx: VecDeque::new(),
        }
    }
}

pub(crate) struct QueueRxToken(Vec<u8>);

impl RxToken for QueueRxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

pub(crate) struct QueueTxToken<'a>(&'a mut VecDeque<Vec<u8>>);

impl TxToken for QueueTxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        self.0.push_back(buf);
        r
    }
}

impl Device for QueueDevice {
    type RxToken<'a> = QueueRxToken;
    type TxToken<'a> = QueueTxToken<'a>;

    fn receive(&mut self, _now: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let pkt = self.rx.pop_front()?;
        Some((QueueRxToken(pkt), QueueTxToken(&mut self.tx)))
    }

    fn transmit(&mut self, _now: Instant) -> Option<Self::TxToken<'_>> {
        if self.tx.len() >= TX_QUEUE_LIMIT {
            return None;
        }
        Some(QueueTxToken(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        // The Ethernet header is added by us, outside the device.
        caps.max_transmission_unit = MTU;
        caps.max_burst_size = None;
        caps
    }
}
