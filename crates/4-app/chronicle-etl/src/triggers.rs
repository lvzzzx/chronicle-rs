use chronicle_protocol::BookEventType;
use chronicle_replay::BookEvent;

use crate::feature::Trigger;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timebase {
    Exchange,
    Ingest,
}

#[derive(Debug, Clone, Copy)]
pub struct TimeBarTrigger {
    interval_ns: u64,
    timebase: Timebase,
    last_bucket: Option<u64>,
}

impl TimeBarTrigger {
    pub fn new(interval_ns: u64, timebase: Timebase) -> Self {
        Self {
            interval_ns: interval_ns.max(1),
            timebase,
            last_bucket: None,
        }
    }
}

impl Trigger for TimeBarTrigger {
    fn should_emit(&mut self, event: &BookEvent<'_>) -> bool {
        let ts = match self.timebase {
            Timebase::Exchange => event.header.exchange_ts_ns,
            Timebase::Ingest => event.header.ingest_ts_ns,
        };
        if ts == 0 {
            return false;
        }
        let bucket = ts / self.interval_ns;
        let emit = self.last_bucket.map_or(true, |prev| bucket > prev);
        if emit {
            self.last_bucket = Some(bucket);
        }
        emit
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EventTypeTrigger {
    event_type: BookEventType,
}

impl EventTypeTrigger {
    pub fn new(event_type: BookEventType) -> Self {
        Self { event_type }
    }
}

impl Trigger for EventTypeTrigger {
    fn should_emit(&mut self, event: &BookEvent<'_>) -> bool {
        event.header.event_type == self.event_type as u8
    }
}

pub struct AnyTrigger {
    triggers: Vec<Box<dyn Trigger>>,
}

impl AnyTrigger {
    pub fn new(triggers: Vec<Box<dyn Trigger>>) -> Self {
        Self { triggers }
    }

    pub fn push(&mut self, trigger: impl Trigger + 'static) {
        self.triggers.push(Box::new(trigger));
    }
}

impl Trigger for AnyTrigger {
    fn should_emit(&mut self, event: &BookEvent<'_>) -> bool {
        for trigger in &mut self.triggers {
            if trigger.should_emit(event) {
                return true;
            }
        }
        false
    }
}
