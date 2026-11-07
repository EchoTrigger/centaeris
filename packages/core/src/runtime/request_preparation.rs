use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestPreparationStage {
    Estimate,
    Compaction,
    InputIntake,
    Projection,
    AgentsInstructionsRead,
}

pub struct RequestPreparationTiming {
    pub stage: RequestPreparationStage,
    pub turn_id: String,
    pub elapsed: Duration,
    pub completed: bool,
}

pub type RequestPreparationObserver = Arc<dyn Fn(&RequestPreparationTiming) + Send + Sync>;

pub(super) struct RequestPreparationSpan<'a> {
    observer: Option<&'a RequestPreparationObserver>,
    stage: RequestPreparationStage,
    turn_id: &'a str,
    started: Option<Instant>,
    completed: bool,
}

impl<'a> RequestPreparationSpan<'a> {
    pub(super) fn new(
        observer: Option<&'a RequestPreparationObserver>,
        stage: RequestPreparationStage,
        turn_id: &'a str,
    ) -> Self {
        Self {
            observer,
            stage,
            turn_id,
            started: observer.map(|_| Instant::now()),
            completed: false,
        }
    }

    pub(super) fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for RequestPreparationSpan<'_> {
    fn drop(&mut self) {
        if let (Some(observer), Some(started)) = (self.observer, self.started) {
            observer(&RequestPreparationTiming {
                stage: self.stage,
                turn_id: self.turn_id.into(),
                elapsed: started.elapsed(),
                completed: self.completed,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn preparation_observer_distinguishes_completed_and_abandoned_scopes() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let captured = records.clone();
        let observer: RequestPreparationObserver = Arc::new(move |timing| {
            captured
                .lock()
                .unwrap()
                .push((timing.stage, timing.turn_id.clone(), timing.completed));
        });
        RequestPreparationSpan::new(Some(&observer), RequestPreparationStage::Estimate, "turn-1")
            .complete();
        drop(RequestPreparationSpan::new(
            Some(&observer),
            RequestPreparationStage::Projection,
            "turn-2",
        ));
        assert_eq!(
            *records.lock().unwrap(),
            [
                (RequestPreparationStage::Estimate, "turn-1".into(), true),
                (RequestPreparationStage::Projection, "turn-2".into(), false),
            ]
        );
    }
}
