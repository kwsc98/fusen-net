// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lock-free runtime counters shared by server and agent tasks.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct RuntimeMetrics {
    control_sessions: AtomicU64,
    relay_sessions: AtomicU64,
    p2p_sessions: AtomicU64,
    relay_packets: AtomicU64,
    p2p_packets: AtomicU64,
    dropped_packets: AtomicU64,
    invalid_packet_drops: AtomicU64,
    queue_full_drops: AtomicU64,
    unavailable_path_drops: AtomicU64,
    transport_drops: AtomicU64,
    path_transitions: AtomicU64,
    enrollment_successes: AtomicU64,
    enrollment_failures: AtomicU64,
    renewal_successes: AtomicU64,
    renewal_failures: AtomicU64,
    p2p_connection_successes: AtomicU64,
    p2p_connection_failures: AtomicU64,
    control_queue_depth_high_watermark: AtomicU64,
    relay_queue_depth_high_watermark: AtomicU64,
    p2p_queue_depth_high_watermark: AtomicU64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketDropReason {
    InvalidPacket,
    QueueFull,
    UnavailablePath,
    Transport,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MetricsSnapshot {
    pub control_sessions: u64,
    pub relay_sessions: u64,
    pub p2p_sessions: u64,
    pub relay_packets: u64,
    pub p2p_packets: u64,
    pub dropped_packets: u64,
    pub invalid_packet_drops: u64,
    pub queue_full_drops: u64,
    pub unavailable_path_drops: u64,
    pub transport_drops: u64,
    pub path_transitions: u64,
    pub enrollment_successes: u64,
    pub enrollment_failures: u64,
    pub renewal_successes: u64,
    pub renewal_failures: u64,
    pub p2p_connection_successes: u64,
    pub p2p_connection_failures: u64,
    pub control_queue_depth_high_watermark: u64,
    pub relay_queue_depth_high_watermark: u64,
    pub p2p_queue_depth_high_watermark: u64,
}

impl RuntimeMetrics {
    pub fn set_control_sessions(&self, value: usize) {
        self.control_sessions.store(value as u64, Ordering::Relaxed);
    }

    pub fn set_relay_sessions(&self, value: usize) {
        self.relay_sessions.store(value as u64, Ordering::Relaxed);
    }

    pub fn set_p2p_sessions(&self, value: usize) {
        self.p2p_sessions.store(value as u64, Ordering::Relaxed);
    }

    pub fn record_relay_packet(&self) {
        self.relay_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_p2p_packet(&self) {
        self.p2p_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_drop(&self) {
        self.record_drops(1);
    }

    pub fn record_drops(&self, count: u64) {
        self.dropped_packets.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_drop_reason(&self, reason: PacketDropReason) {
        self.record_drops_by_reason(reason, 1);
    }

    pub fn record_drops_by_reason(&self, reason: PacketDropReason, count: u64) {
        self.record_drops(count);
        let counter = match reason {
            PacketDropReason::InvalidPacket => &self.invalid_packet_drops,
            PacketDropReason::QueueFull => &self.queue_full_drops,
            PacketDropReason::UnavailablePath => &self.unavailable_path_drops,
            PacketDropReason::Transport => &self.transport_drops,
        };
        counter.fetch_add(count, Ordering::Relaxed);
    }

    pub fn record_path_transition(&self) {
        self.path_transitions.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_enrollment(&self, success: bool) {
        let counter = if success {
            &self.enrollment_successes
        } else {
            &self.enrollment_failures
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_renewal(&self, success: bool) {
        let counter = if success {
            &self.renewal_successes
        } else {
            &self.renewal_failures
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_p2p_connection(&self, success: bool) {
        let counter = if success {
            &self.p2p_connection_successes
        } else {
            &self.p2p_connection_failures
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_control_queue_depth(&self, depth: usize) {
        observe_high_watermark(&self.control_queue_depth_high_watermark, depth);
    }

    pub fn observe_relay_queue_depth(&self, depth: usize) {
        observe_high_watermark(&self.relay_queue_depth_high_watermark, depth);
    }

    pub fn observe_p2p_queue_depth(&self, depth: usize) {
        observe_high_watermark(&self.p2p_queue_depth_high_watermark, depth);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            control_sessions: self.control_sessions.load(Ordering::Relaxed),
            relay_sessions: self.relay_sessions.load(Ordering::Relaxed),
            p2p_sessions: self.p2p_sessions.load(Ordering::Relaxed),
            relay_packets: self.relay_packets.load(Ordering::Relaxed),
            p2p_packets: self.p2p_packets.load(Ordering::Relaxed),
            dropped_packets: self.dropped_packets.load(Ordering::Relaxed),
            invalid_packet_drops: self.invalid_packet_drops.load(Ordering::Relaxed),
            queue_full_drops: self.queue_full_drops.load(Ordering::Relaxed),
            unavailable_path_drops: self.unavailable_path_drops.load(Ordering::Relaxed),
            transport_drops: self.transport_drops.load(Ordering::Relaxed),
            path_transitions: self.path_transitions.load(Ordering::Relaxed),
            enrollment_successes: self.enrollment_successes.load(Ordering::Relaxed),
            enrollment_failures: self.enrollment_failures.load(Ordering::Relaxed),
            renewal_successes: self.renewal_successes.load(Ordering::Relaxed),
            renewal_failures: self.renewal_failures.load(Ordering::Relaxed),
            p2p_connection_successes: self.p2p_connection_successes.load(Ordering::Relaxed),
            p2p_connection_failures: self.p2p_connection_failures.load(Ordering::Relaxed),
            control_queue_depth_high_watermark: self
                .control_queue_depth_high_watermark
                .load(Ordering::Relaxed),
            relay_queue_depth_high_watermark: self
                .relay_queue_depth_high_watermark
                .load(Ordering::Relaxed),
            p2p_queue_depth_high_watermark: self
                .p2p_queue_depth_high_watermark
                .load(Ordering::Relaxed),
        }
    }

    /// Renders a point-in-time Prometheus text exposition without labels or
    /// identity-bearing values.
    pub fn encode_prometheus(&self) -> String {
        self.snapshot().encode_prometheus()
    }
}

impl MetricsSnapshot {
    pub fn encode_prometheus(self) -> String {
        let mut output = String::with_capacity(2_048);
        for (name, metric_type, value) in [
            ("control_sessions", "gauge", self.control_sessions),
            ("relay_sessions", "gauge", self.relay_sessions),
            ("p2p_sessions", "gauge", self.p2p_sessions),
            ("relay_packets_total", "counter", self.relay_packets),
            ("p2p_packets_total", "counter", self.p2p_packets),
            ("dropped_packets_total", "counter", self.dropped_packets),
            (
                "invalid_packet_drops_total",
                "counter",
                self.invalid_packet_drops,
            ),
            ("queue_full_drops_total", "counter", self.queue_full_drops),
            (
                "unavailable_path_drops_total",
                "counter",
                self.unavailable_path_drops,
            ),
            ("transport_drops_total", "counter", self.transport_drops),
            ("path_transitions_total", "counter", self.path_transitions),
            (
                "enrollment_successes_total",
                "counter",
                self.enrollment_successes,
            ),
            (
                "enrollment_failures_total",
                "counter",
                self.enrollment_failures,
            ),
            ("renewal_successes_total", "counter", self.renewal_successes),
            ("renewal_failures_total", "counter", self.renewal_failures),
            (
                "p2p_connection_successes_total",
                "counter",
                self.p2p_connection_successes,
            ),
            (
                "p2p_connection_failures_total",
                "counter",
                self.p2p_connection_failures,
            ),
            (
                "control_queue_depth_high_watermark",
                "gauge",
                self.control_queue_depth_high_watermark,
            ),
            (
                "relay_queue_depth_high_watermark",
                "gauge",
                self.relay_queue_depth_high_watermark,
            ),
            (
                "p2p_queue_depth_high_watermark",
                "gauge",
                self.p2p_queue_depth_high_watermark,
            ),
        ] {
            use std::fmt::Write as _;
            let _ = writeln!(output, "# TYPE stellaris_{name} {metric_type}");
            let _ = writeln!(output, "stellaris_{name} {value}");
        }
        output
    }
}

fn observe_high_watermark(counter: &AtomicU64, depth: usize) {
    counter.fetch_max(depth as u64, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reports_each_counter() {
        let metrics = RuntimeMetrics::default();
        metrics.set_control_sessions(2);
        metrics.set_relay_sessions(1);
        metrics.set_p2p_sessions(1);
        metrics.record_relay_packet();
        metrics.record_p2p_packet();
        metrics.record_drop();
        metrics.record_drop_reason(PacketDropReason::InvalidPacket);
        metrics.record_drop_reason(PacketDropReason::QueueFull);
        metrics.record_drop_reason(PacketDropReason::UnavailablePath);
        metrics.record_drop_reason(PacketDropReason::Transport);
        metrics.record_path_transition();
        metrics.record_enrollment(true);
        metrics.record_enrollment(false);
        metrics.record_renewal(true);
        metrics.record_renewal(false);
        metrics.record_p2p_connection(true);
        metrics.record_p2p_connection(false);
        metrics.observe_control_queue_depth(2);
        metrics.observe_control_queue_depth(1);
        metrics.observe_relay_queue_depth(3);
        metrics.observe_p2p_queue_depth(4);
        assert_eq!(
            metrics.snapshot(),
            MetricsSnapshot {
                control_sessions: 2,
                relay_sessions: 1,
                p2p_sessions: 1,
                relay_packets: 1,
                p2p_packets: 1,
                dropped_packets: 5,
                invalid_packet_drops: 1,
                queue_full_drops: 1,
                unavailable_path_drops: 1,
                transport_drops: 1,
                path_transitions: 1,
                enrollment_successes: 1,
                enrollment_failures: 1,
                renewal_successes: 1,
                renewal_failures: 1,
                p2p_connection_successes: 1,
                p2p_connection_failures: 1,
                control_queue_depth_high_watermark: 2,
                relay_queue_depth_high_watermark: 3,
                p2p_queue_depth_high_watermark: 4,
            }
        );
    }

    #[test]
    fn prometheus_output_is_bounded_and_contains_no_identity_labels() {
        let metrics = RuntimeMetrics::default();
        metrics.set_control_sessions(2);
        metrics.record_drop_reason(PacketDropReason::Transport);

        let output = metrics.encode_prometheus();
        assert!(output.len() < 4_096);
        assert!(output.contains("# TYPE stellaris_control_sessions gauge\n"));
        assert!(output.contains("stellaris_control_sessions 2\n"));
        assert!(output.contains("stellaris_transport_drops_total 1\n"));
        assert!(!output.contains('{'));
    }
}
