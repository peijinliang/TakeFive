use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindowBuilder,
};

pub(crate) const REMINDER_SURFACE_LABEL: &str = "reminder";
const REMINDER_SURFACE_EVENT: &str = "reminder-surface-updated";
const REMINDER_SURFACE_WIDTH: f64 = 326.0;
const REMINDER_SURFACE_HEIGHT: f64 = 194.0;
const REMINDER_SURFACE_MARGIN: f64 = 12.0;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReminderSurfacePayload {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) occurrence_id: String,
    pub(crate) scheduled_at: String,
}

#[derive(Clone, Default)]
pub(crate) struct ReminderSurfaceState {
    queue: Arc<RwLock<VecDeque<ReminderSurfacePayload>>>,
    presentation: Arc<Mutex<()>>,
}

#[derive(Debug, PartialEq, Eq)]
enum QueueAdvance {
    Unchanged,
    Empty,
    Next(ReminderSurfacePayload),
}

impl ReminderSurfaceState {
    pub(crate) fn latest(&self) -> Result<Option<ReminderSurfacePayload>, String> {
        self.queue
            .read()
            .map(|queue| queue.front().cloned())
            .map_err(|_| "reminder_surface_payload_state_poisoned".to_string())
    }

    fn enqueue(&self, payload: ReminderSurfacePayload) -> Result<bool, String> {
        let mut queue = self
            .queue
            .write()
            .map_err(|_| "reminder_surface_payload_state_poisoned".to_string())?;
        if queue
            .iter()
            .any(|queued| queued.occurrence_id == payload.occurrence_id)
        {
            return Ok(false);
        }
        let became_current = queue.is_empty();
        queue.push_back(payload);
        Ok(became_current)
    }

    fn advance(&self, occurrence_id: &str) -> Result<QueueAdvance, String> {
        let mut queue = self
            .queue
            .write()
            .map_err(|_| "reminder_surface_payload_state_poisoned".to_string())?;
        let Some(index) = queue
            .iter()
            .position(|payload| payload.occurrence_id == occurrence_id)
        else {
            return Ok(QueueAdvance::Unchanged);
        };
        queue.remove(index);
        if index != 0 {
            return Ok(QueueAdvance::Unchanged);
        }
        Ok(match queue.front().cloned() {
            Some(next) => QueueAdvance::Next(next),
            None => QueueAdvance::Empty,
        })
    }

    fn show_payload(app: &AppHandle, payload: ReminderSurfacePayload) -> Result<(), String> {
        let (x, y) = surface_position(app).unwrap_or((24.0, 24.0));
        if let Some(window) = app.get_webview_window(REMINDER_SURFACE_LABEL) {
            window
                .set_size(LogicalSize::new(
                    REMINDER_SURFACE_WIDTH,
                    REMINDER_SURFACE_HEIGHT,
                ))
                .map_err(|error| format!("reminder_surface_resize_failed: {error}"))?;
            window
                .set_position(LogicalPosition::new(x, y))
                .map_err(|error| format!("reminder_surface_reposition_failed: {error}"))?;
            window
                .show()
                .map_err(|error| format!("reminder_surface_show_failed: {error}"))?;
            window
                .emit(REMINDER_SURFACE_EVENT, payload)
                .map_err(|error| format!("reminder_surface_update_failed: {error}"))?;
            return Ok(());
        }

        WebviewWindowBuilder::new(
            app,
            REMINDER_SURFACE_LABEL,
            WebviewUrl::App("index.html?surface=reminder".into()),
        )
        .title("摸个鱼提醒")
        .inner_size(REMINDER_SURFACE_WIDTH, REMINDER_SURFACE_HEIGHT)
        .min_inner_size(REMINDER_SURFACE_WIDTH, REMINDER_SURFACE_HEIGHT)
        .max_inner_size(REMINDER_SURFACE_WIDTH, REMINDER_SURFACE_HEIGHT)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(true)
        .transparent(true)
        .position(x, y)
        .build()
        .map(|_| ())
        .map_err(|error| format!("reminder_surface_create_failed: {error}"))
    }

    pub(crate) fn present(
        &self,
        app: &AppHandle,
        payload: ReminderSurfacePayload,
    ) -> Result<(), String> {
        let _presentation = self
            .presentation
            .lock()
            .map_err(|_| "reminder_surface_presentation_state_poisoned".to_string())?;
        if !self.enqueue(payload.clone())? {
            return Ok(());
        }

        if let Err(error) = Self::show_payload(app, payload.clone()) {
            let _ = self.advance(&payload.occurrence_id);
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn finish(&self, app: &AppHandle, occurrence_id: &str) -> Result<(), String> {
        let _presentation = self
            .presentation
            .lock()
            .map_err(|_| "reminder_surface_presentation_state_poisoned".to_string())?;
        match self.advance(occurrence_id)? {
            QueueAdvance::Unchanged => Ok(()),
            QueueAdvance::Next(next) => Self::show_payload(app, next),
            QueueAdvance::Empty => {
                if let Some(window) = app.get_webview_window(REMINDER_SURFACE_LABEL) {
                    window
                        .hide()
                        .map_err(|error| format!("reminder_surface_hide_failed: {error}"))?;
                }
                Ok(())
            }
        }
    }
}

fn surface_position(app: &AppHandle) -> Option<(f64, f64)> {
    let monitor = app.primary_monitor().ok().flatten()?;
    let scale_factor = monitor.scale_factor();
    let work_area = monitor.work_area();
    let position = work_area.position.to_logical::<f64>(scale_factor);
    let size = work_area.size.to_logical::<f64>(scale_factor);
    Some(bottom_right_position(
        position.x,
        position.y,
        size.width,
        size.height,
    ))
}

fn bottom_right_position(x: f64, y: f64, width: f64, height: f64) -> (f64, f64) {
    (
        x + width - REMINDER_SURFACE_WIDTH - REMINDER_SURFACE_MARGIN,
        y + height - REMINDER_SURFACE_HEIGHT - REMINDER_SURFACE_MARGIN,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(id: &str, title: &str) -> ReminderSurfacePayload {
        ReminderSurfacePayload {
            title: title.to_string(),
            body: "休息一下".to_string(),
            occurrence_id: id.to_string(),
            scheduled_at: "2026-07-14T08:00:00+00:00".to_string(),
        }
    }

    #[test]
    fn payload_state_starts_empty_and_returns_a_clone() {
        let state = ReminderSurfaceState::default();
        assert_eq!(state.latest().unwrap(), None);

        let expected = payload("occurrence-1", "喝水");
        assert!(state.enqueue(expected.clone()).unwrap());
        assert_eq!(state.latest().unwrap(), Some(expected));
    }

    #[test]
    fn simultaneous_payloads_are_queued_and_advanced_in_order() {
        let state = ReminderSurfaceState::default();
        let first = payload("occurrence-1", "喝水");
        let second = payload("occurrence-2", "走动");
        assert!(state.enqueue(first.clone()).unwrap());
        assert!(!state.enqueue(second.clone()).unwrap());

        assert_eq!(state.latest().unwrap(), Some(first));
        assert_eq!(
            state.advance("occurrence-1").unwrap(),
            QueueAdvance::Next(second.clone())
        );
        assert_eq!(state.latest().unwrap(), Some(second));
        assert_eq!(state.advance("occurrence-2").unwrap(), QueueAdvance::Empty);
        assert_eq!(state.latest().unwrap(), None);
    }

    #[test]
    fn duplicate_occurrence_is_not_queued_twice() {
        let state = ReminderSurfaceState::default();
        let expected = payload("occurrence-1", "喝水");
        assert!(state.enqueue(expected.clone()).unwrap());
        assert!(!state.enqueue(expected.clone()).unwrap());

        assert_eq!(state.latest().unwrap(), Some(expected));
        assert_eq!(state.advance("occurrence-1").unwrap(), QueueAdvance::Empty);
        assert_eq!(
            state.advance("occurrence-1").unwrap(),
            QueueAdvance::Unchanged
        );
    }

    #[test]
    fn surface_is_placed_inside_the_bottom_right_work_area() {
        assert_eq!(
            bottom_right_position(0.0, 0.0, 1920.0, 1040.0),
            (1582.0, 834.0)
        );
        assert_eq!(
            bottom_right_position(-1280.0, 40.0, 1280.0, 984.0),
            (-338.0, 818.0)
        );
    }
}
