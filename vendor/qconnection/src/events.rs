use std::sync::Arc;

use qbase::{
    self,
    error::{AppError, Error, ErrorKind, QuicError},
    frame::ConnectionCloseFrame,
};
use qevent::quic::connectivity::BaseConnectionStates;
use tokio::sync::mpsc;

use crate::state::ArcConnState;

pub(crate) const CONNECTION_EVENT_QUEUE_CAPACITY: usize = 256;

/// The events that can be emitted by a quic connection
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    // The connection is handshaked
    Handshaked,
    // An Error occurred during the connection, will enter the closing state
    Failed(QuicError),
    // The connection is closed by application, just a notification
    ApplicationClose(AppError),
    // Received a connection close frame, will enter the draining state
    Closed(ConnectionCloseFrame),
    // Received a stateless reset, will enter the draining state
    StatelessReset,
    // The connection is terminated completely
    Terminated,
}

pub trait EmitEvent: Send + Sync {
    fn emit(&self, event: Event);
}

#[derive(Clone)]
pub struct ArcEventBroker {
    conn_state: ArcConnState,
    raw_broker: Arc<dyn EmitEvent>,
}

impl ArcEventBroker {
    pub fn new<E: EmitEvent + 'static>(conn_state: ArcConnState, event_broker: E) -> Self {
        Self {
            conn_state,
            raw_broker: Arc::new(event_broker),
        }
    }
}

impl EmitEvent for ArcEventBroker {
    fn emit(&self, event: Event) {
        match &event {
            Event::Handshaked => {
                if self.conn_state.enter_handshaked().is_none() {
                    return;
                }
            }
            Event::Failed(error) => {
                if self.conn_state.enter_closing(error).is_none() {
                    return;
                }
            }
            Event::ApplicationClose(error) => {
                if self.conn_state.enter_closing(error).is_none() {
                    return;
                }
            }
            Event::Closed(ccf) => {
                if self.conn_state.enter_draining(ccf).is_none() {
                    return;
                }
            }
            Event::Terminated => {
                let terminated_state = BaseConnectionStates::Closed;
                self.conn_state.update(terminated_state.into());
            }
            Event::StatelessReset => {
                let error = QuicError::with_default_fty(ErrorKind::None, "stateless reset");
                let close_frame = ConnectionCloseFrame::from(Error::from(error));
                if self.conn_state.enter_draining(&close_frame).is_none() {
                    return;
                }
            }
        };
        tracing::debug!(target: "quic", new_state = ?event, "Connection state changed");
        self.raw_broker.emit(event);
    }
}

impl EmitEvent for mpsc::Sender<Event> {
    fn emit(&self, event: Event) {
        match self.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => tracing::warn!(
                target: "quic",
                "connection event queue reached its {CONNECTION_EVENT_QUEUE_CAPACITY}-event limit"
            ),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!(target: "quic", "connection event consumer stopped");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;

    #[test]
    fn test_emit_event() {
        let (tx, mut rx) = mpsc::channel(CONNECTION_EVENT_QUEUE_CAPACITY);
        tx.emit(Event::Handshaked);
        assert_eq!(rx.try_recv().unwrap(), Event::Handshaked);
    }

    #[test]
    fn stateless_reset_enters_draining_without_panicking() {
        let state = ArcConnState::new();
        let (tx, mut rx) = mpsc::channel(CONNECTION_EVENT_QUEUE_CAPACITY);
        let broker = ArcEventBroker::new(state.clone(), tx);

        broker.emit(Event::StatelessReset);

        assert_eq!(state.current(), Some(crate::state::DRAINING));
        assert_eq!(rx.try_recv().unwrap(), Event::StatelessReset);
    }
}
