use crate::types::OrbitEvent;
use tokio::sync::mpsc;

pub type EventSink = mpsc::UnboundedSender<OrbitEvent>;
pub type EventStream = mpsc::UnboundedReceiver<OrbitEvent>;

pub fn channel() -> (EventSink, EventStream) {
    mpsc::unbounded_channel()
}

macro_rules! emit {
    ($sender:expr, $event:expr) => {
        if let crate::types::OrbitEvent::PhaseChanged(ref __phase) = $event {
            tracing::info!(phase = ?__phase, "phase changed");
        }
        let _result = $sender.send($event);
    };
}

pub(crate) use emit;
