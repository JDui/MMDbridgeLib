use std::{
    sync::{Condvar, Mutex, OnceLock},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{CoreError, CoreResult};

const MAX_STAGE_CONCURRENCY: u8 = 8;
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailConcurrencySettings {
    pub parse: Option<u8>,
    pub render: Option<u8>,
    pub encode: Option<u8>,
}

impl Default for ThumbnailConcurrencySettings {
    fn default() -> Self {
        Self {
            parse: None,
            render: None,
            encode: None,
        }
    }
}

impl ThumbnailConcurrencySettings {
    pub(crate) fn validate(&self) -> CoreResult<()> {
        for (stage, value) in [
            ("Parse", self.parse),
            ("Render", self.render),
            ("Encode", self.encode),
        ] {
            if value.is_some_and(|value| !(1..=MAX_STAGE_CONCURRENCY).contains(&value)) {
                return Err(CoreError::ThumbnailQueue(format!(
                    "{stage} 并发数必须在 1 到 {MAX_STAGE_CONCURRENCY} 之间"
                )));
            }
        }
        Ok(())
    }

    fn resolved(&self) -> [usize; 3] {
        let automatic_cpu_limit = std::thread::available_parallelism()
            .map(|count| count.get().saturating_sub(1).clamp(1, 8))
            .unwrap_or(2);
        [
            self.parse.map(usize::from).unwrap_or(automatic_cpu_limit),
            self.render.map(usize::from).unwrap_or(1),
            self.encode.map(usize::from).unwrap_or(automatic_cpu_limit),
        ]
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ThumbnailStage {
    Parse,
    Render,
    Encode,
}

impl ThumbnailStage {
    fn index(self) -> usize {
        match self {
            Self::Parse => 0,
            Self::Render => 1,
            Self::Encode => 2,
        }
    }
}

struct ControllerState {
    settings: ThumbnailConcurrencySettings,
    active: [usize; 3],
}

struct Controller {
    state: Mutex<ControllerState>,
    changed: Condvar,
}

static CONTROLLER: OnceLock<Controller> = OnceLock::new();

fn controller() -> &'static Controller {
    CONTROLLER.get_or_init(|| Controller {
        state: Mutex::new(ControllerState {
            settings: ThumbnailConcurrencySettings::default(),
            active: [0; 3],
        }),
        changed: Condvar::new(),
    })
}

pub(crate) fn configure(settings: ThumbnailConcurrencySettings) {
    if let Ok(mut state) = controller().state.lock() {
        state.settings = settings;
        controller().changed.notify_all();
    }
}

pub(crate) fn acquire(
    stage: ThumbnailStage,
    mut on_wait: impl FnMut() -> bool,
) -> CoreResult<StagePermit> {
    let controller = controller();
    let index = stage.index();
    let mut state = controller
        .state
        .lock()
        .map_err(|_| CoreError::LockPoisoned)?;
    loop {
        let limit = state.settings.resolved()[index];
        if state.active[index] < limit {
            state.active[index] += 1;
            return Ok(StagePermit { stage });
        }
        let (next_state, timeout) = controller
            .changed
            .wait_timeout(state, WAIT_POLL_INTERVAL)
            .map_err(|_| CoreError::LockPoisoned)?;
        state = next_state;
        if timeout.timed_out() {
            drop(state);
            if !on_wait() {
                return Err(CoreError::ThumbnailCancelled);
            }
            state = controller
                .state
                .lock()
                .map_err(|_| CoreError::LockPoisoned)?;
        }
    }
}

pub(crate) struct StagePermit {
    stage: ThumbnailStage,
}

impl Drop for StagePermit {
    fn drop(&mut self) {
        let controller = controller();
        if let Ok(mut state) = controller.state.lock() {
            let index = self.stage.index();
            state.active[index] = state.active[index].saturating_sub(1);
            controller.changed.notify_all();
        }
    }
}
