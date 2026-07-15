use chrono::Utc;
use serde::Serialize;

#[cfg(target_os = "windows")]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Mutex, OnceLock,
};

#[cfg(target_os = "windows")]
use tauri::{AppHandle, Emitter};

#[cfg(target_os = "windows")]
static LIFECYCLE_SENDER: OnceLock<mpsc::Sender<LifecycleEvent>> = OnceLock::new();

#[cfg(target_os = "windows")]
static LIFECYCLE_MONITOR_STARTED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
static LIFECYCLE_MONITOR_ERROR: OnceLock<String> = OnceLock::new();

#[cfg(target_os = "windows")]
static TIME_ZONE_STATE: OnceLock<Mutex<Option<TimeZoneSnapshot>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleEvent {
    kind: &'static str,
    observed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<serde_json::Value>,
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct TimeZoneSnapshot {
    bias: i32,
    standard_name: String,
    standard_date: [u16; 8],
    standard_bias: i32,
    daylight_name: String,
    daylight_date: [u16; 8],
    daylight_bias: i32,
    key_name: String,
    dynamic_daylight_time_disabled: bool,
}

impl LifecycleEvent {
    #[cfg(target_os = "windows")]
    fn observed(kind: &'static str, detail: Option<serde_json::Value>) -> Self {
        Self {
            kind,
            observed_at: Utc::now().to_rfc3339(),
            detail,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformProbe {
    platform: String,
    architecture: String,
    checked_at: String,
    idle_duration_ms: Option<u64>,
    display_count: Option<i32>,
    pub(crate) foreground_fullscreen: Option<bool>,
    capabilities: Vec<CapabilityProbe>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapabilityProbe {
    key: &'static str,
    label: &'static str,
    status: &'static str,
    detail: String,
}

pub fn probe() -> PlatformProbe {
    #[cfg(target_os = "windows")]
    {
        windows_probe()
    }

    #[cfg(not(target_os = "windows"))]
    PlatformProbe {
        platform: std::env::consts::OS.to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        checked_at: Utc::now().to_rfc3339(),
        idle_duration_ms: None,
        display_count: None,
        foreground_fullscreen: None,
        capabilities: vec![CapabilityProbe {
            key: "native-adapter",
            label: "原生平台适配",
            status: "pending",
            detail: "当前平台尚未在本机验证".to_string(),
        }],
    }
}

#[cfg(target_os = "windows")]
pub fn start_lifecycle_monitor(app: AppHandle) -> Result<(), String> {
    let result = start_lifecycle_monitor_inner(app);
    if let Err(error) = &result {
        let _ = LIFECYCLE_MONITOR_ERROR.set(error.clone());
    }
    result
}

#[cfg(target_os = "windows")]
fn start_lifecycle_monitor_inner(app: AppHandle) -> Result<(), String> {
    if LIFECYCLE_MONITOR_STARTED.load(Ordering::Acquire) {
        return Ok(());
    }

    let (event_sender, event_receiver) = mpsc::channel();
    LIFECYCLE_SENDER
        .set(event_sender)
        .map_err(|_| "Windows lifecycle monitor already initialized".to_string())?;

    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("takefive-win-lifecycle".to_string())
        .spawn(move || run_windows_message_loop(ready_sender))
        .map_err(|error| error.to_string())?;

    ready_receiver
        .recv_timeout(std::time::Duration::from_secs(3))
        .map_err(|error| format!("Windows lifecycle monitor startup timed out: {error}"))??;

    LIFECYCLE_MONITOR_STARTED.store(true, Ordering::Release);
    std::thread::Builder::new()
        .name("takefive-lifecycle-events".to_string())
        .spawn(move || {
            while let Ok(event) = event_receiver.recv() {
                let _ = app.emit("native-lifecycle-event", event);
            }
        })
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn start_lifecycle_monitor(_app: tauri::AppHandle) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_probe() -> PlatformProbe {
    use std::mem::size_of;
    use windows::Win32::{
        Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        },
        System::SystemInformation::GetTickCount64,
        UI::{
            Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
            WindowsAndMessaging::{
                GetForegroundWindow, GetSystemMetrics, GetWindowRect, SM_CMONITORS,
            },
        },
    };

    let idle_duration_ms = unsafe {
        let mut last_input = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        GetLastInputInfo(&mut last_input)
            .as_bool()
            .then(|| GetTickCount64().saturating_sub(last_input.dwTime as u64))
    };

    let detected_display_count = unsafe { GetSystemMetrics(SM_CMONITORS) };
    let display_count = Some(detected_display_count);
    let foreground_fullscreen = unsafe {
        let window = GetForegroundWindow();
        if window.0.is_null() {
            None
        } else {
            let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);
            let mut window_rect = Default::default();
            let mut monitor_info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetWindowRect(window, &mut window_rect).is_ok()
                && GetMonitorInfoW(monitor, &mut monitor_info).as_bool()
            {
                let bounds = monitor_info.rcMonitor;
                let tolerance = 2;
                Some(
                    (window_rect.left - bounds.left).abs() <= tolerance
                        && (window_rect.top - bounds.top).abs() <= tolerance
                        && (window_rect.right - bounds.right).abs() <= tolerance
                        && (window_rect.bottom - bounds.bottom).abs() <= tolerance,
                )
            } else {
                None
            }
        }
    };

    let capabilities = vec![
        CapabilityProbe {
            key: "idle-time",
            label: "系统空闲时长",
            status: if idle_duration_ms.is_some() {
                "available"
            } else {
                "unavailable"
            },
            detail: idle_duration_ms
                .map(|value| format!("当前 {} 秒", value / 1000))
                .unwrap_or_else(|| "GetLastInputInfo 调用失败".to_string()),
        },
        CapabilityProbe {
            key: "foreground-fullscreen",
            label: "前台全屏判断",
            status: if foreground_fullscreen.is_some() {
                "available"
            } else {
                "unknown"
            },
            detail: match foreground_fullscreen {
                Some(true) => "当前前台窗口覆盖活动显示器".to_string(),
                Some(false) => "当前前台窗口未覆盖活动显示器".to_string(),
                None => "无法读取前台窗口边界".to_string(),
            },
        },
        CapabilityProbe {
            key: "display-enumeration",
            label: "多显示器枚举",
            status: "available",
            detail: format!("检测到 {detected_display_count} 台显示器"),
        },
        CapabilityProbe {
            key: "sleep-wake-events",
            label: "休眠与唤醒事件",
            status: if LIFECYCLE_MONITOR_STARTED.load(Ordering::Acquire) {
                "available"
            } else {
                "pending"
            },
            detail: if LIFECYCLE_MONITOR_STARTED.load(Ordering::Acquire) {
                "Windows 电源广播监听已启动，等待实机休眠演练".to_string()
            } else {
                "Windows 电源广播监听尚未启动".to_string()
            },
        },
        CapabilityProbe {
            key: "session-lock-events",
            label: "锁屏与解锁事件",
            status: if LIFECYCLE_MONITOR_STARTED.load(Ordering::Acquire) {
                "available"
            } else {
                "pending"
            },
            detail: if LIFECYCLE_MONITOR_STARTED.load(Ordering::Acquire) {
                "WTS 会话通知监听已启动，等待实机锁屏演练".to_string()
            } else {
                "WTS 会话通知监听尚未启动".to_string()
            },
        },
        CapabilityProbe {
            key: "clock-change-events",
            label: "系统时间变化事件",
            status: lifecycle_monitor_status(),
            detail: lifecycle_monitor_detail(
                "WM_TIMECHANGE 监听已启动，等待实机修改系统时间演练",
                "Windows 系统时间广播监听尚未启动",
            ),
        },
        CapabilityProbe {
            key: "timezone-change-events",
            label: "时区变化事件",
            status: lifecycle_monitor_status(),
            detail: lifecycle_monitor_detail(
                "WM_SETTINGCHANGE 到达时会核对动态时区快照，等待实机切换时区演练",
                "Windows 时区变化监听尚未启动",
            ),
        },
        CapabilityProbe {
            key: "display-change-events",
            label: "显示器变化事件",
            status: lifecycle_monitor_status(),
            detail: lifecycle_monitor_detail(
                "WM_DISPLAYCHANGE 监听已启动，等待实机拔插或分辨率变化演练",
                "Windows 显示器变化广播监听尚未启动",
            ),
        },
    ];

    PlatformProbe {
        platform: "Windows".to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        checked_at: Utc::now().to_rfc3339(),
        idle_duration_ms,
        display_count,
        foreground_fullscreen,
        capabilities,
    }
}

#[cfg(target_os = "windows")]
fn lifecycle_monitor_status() -> &'static str {
    if LIFECYCLE_MONITOR_ERROR.get().is_some() {
        "unavailable"
    } else if LIFECYCLE_MONITOR_STARTED.load(Ordering::Acquire) {
        "available"
    } else {
        "pending"
    }
}

#[cfg(target_os = "windows")]
fn lifecycle_monitor_detail(started: &str, pending: &str) -> String {
    if let Some(error) = LIFECYCLE_MONITOR_ERROR.get() {
        format!("Windows 生命周期监听不可用：{error}")
    } else if LIFECYCLE_MONITOR_STARTED.load(Ordering::Acquire) {
        started.to_string()
    } else {
        pending.to_string()
    }
}

#[cfg(target_os = "windows")]
fn run_windows_message_loop(ready: mpsc::SyncSender<Result<(), String>>) {
    use windows::{
        core::w,
        Win32::{
            Foundation::HINSTANCE,
            System::{
                LibraryLoader::GetModuleHandleW,
                RemoteDesktop::{WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION},
            },
            UI::WindowsAndMessaging::{
                CreateWindowExW, DispatchMessageW, GetMessageW, RegisterClassW, TranslateMessage,
                MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
            },
        },
    };

    let initialization = (|| -> Result<(), String> {
        unsafe {
            let module = GetModuleHandleW(None).map_err(|error| error.to_string())?;
            let instance = HINSTANCE(module.0);
            let class_name = w!("TakeFiveLifecycleMessageWindow");
            let window_class = WNDCLASSW {
                lpfnWndProc: Some(lifecycle_window_proc),
                hInstance: instance,
                lpszClassName: class_name,
                ..Default::default()
            };

            if RegisterClassW(&window_class) == 0 {
                return Err("Unable to register lifecycle message window".to_string());
            }

            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!("TakeFive lifecycle listener"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
            .map_err(|error| error.to_string())?;
            WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION)
                .map_err(|error| error.to_string())?;

            let time_zone_state = TIME_ZONE_STATE.get_or_init(|| Mutex::new(None));
            if let Ok(mut state) = time_zone_state.lock() {
                *state = read_time_zone_snapshot();
            }
            Ok(())
        }
    })();

    let initialized = initialization.is_ok();
    if ready.send(initialization).is_err() || !initialized {
        return;
    }

    let mut message = MSG::default();
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn lifecycle_window_proc(
    window: windows::Win32::Foundation::HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetSystemMetrics, SM_CMONITORS, WM_DISPLAYCHANGE, WM_SETTINGCHANGE,
        WM_TIMECHANGE,
    };

    let event = match message {
        WM_TIMECHANGE => Some(LifecycleEvent::observed(
            "time_changed",
            Some(serde_json::json!({ "source": "WM_TIMECHANGE" })),
        )),
        WM_SETTINGCHANGE => detect_time_zone_change().map(|(previous, current)| {
            LifecycleEvent::observed(
                "timezone_changed",
                Some(serde_json::json!({
                    "source": "WM_SETTINGCHANGE",
                    "previousWindowsZone": previous.display_name(),
                    "windowsZone": current.display_name(),
                })),
            )
        }),
        WM_DISPLAYCHANGE => {
            let (width, height) = display_dimensions(lparam.0);
            Some(LifecycleEvent::observed(
                "display_changed",
                Some(serde_json::json!({
                    "source": "WM_DISPLAYCHANGE",
                    "bitsPerPixel": wparam.0 as u32,
                    "width": width,
                    "height": height,
                    "displayCount": GetSystemMetrics(SM_CMONITORS),
                })),
            ))
        }
        _ => lifecycle_event_kind(message, wparam.0 as u32)
            .map(|kind| LifecycleEvent::observed(kind, None)),
    };

    if let (Some(event), Some(sender)) = (event, LIFECYCLE_SENDER.get()) {
        let _ = sender.send(event);
    }

    DefWindowProcW(window, message, wparam, lparam)
}

#[cfg(target_os = "windows")]
fn lifecycle_event_kind(message: u32, parameter: u32) -> Option<&'static str> {
    use windows::Win32::UI::WindowsAndMessaging::{
        PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, WM_POWERBROADCAST,
        WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };

    match (message, parameter) {
        (WM_POWERBROADCAST, PBT_APMSUSPEND) => Some("sleep"),
        (WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND) => Some("wake"),
        (WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK) => Some("lock"),
        (WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK) => Some("unlock"),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn display_dimensions(parameter: isize) -> (u16, u16) {
    let packed = parameter as u32;
    (packed as u16, (packed >> 16) as u16)
}

#[cfg(target_os = "windows")]
fn detect_time_zone_change() -> Option<(TimeZoneSnapshot, TimeZoneSnapshot)> {
    let current = read_time_zone_snapshot()?;
    let state = TIME_ZONE_STATE.get_or_init(|| Mutex::new(None));
    let mut previous = state.lock().ok()?;

    let change = previous
        .as_ref()
        .filter(|snapshot| **snapshot != current)
        .cloned()
        .map(|snapshot| (snapshot, current.clone()));
    *previous = Some(current);
    change
}

#[cfg(target_os = "windows")]
fn read_time_zone_snapshot() -> Option<TimeZoneSnapshot> {
    use windows::Win32::System::Time::{
        GetDynamicTimeZoneInformation, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID,
    };

    let mut information = DYNAMIC_TIME_ZONE_INFORMATION::default();
    if unsafe { GetDynamicTimeZoneInformation(&mut information) } == TIME_ZONE_ID_INVALID {
        return None;
    }

    Some(TimeZoneSnapshot {
        bias: information.Bias,
        standard_name: wide_string(&information.StandardName),
        standard_date: system_time_fields(information.StandardDate),
        standard_bias: information.StandardBias,
        daylight_name: wide_string(&information.DaylightName),
        daylight_date: system_time_fields(information.DaylightDate),
        daylight_bias: information.DaylightBias,
        key_name: wide_string(&information.TimeZoneKeyName),
        dynamic_daylight_time_disabled: information.DynamicDaylightTimeDisabled,
    })
}

#[cfg(target_os = "windows")]
fn system_time_fields(value: windows::Win32::Foundation::SYSTEMTIME) -> [u16; 8] {
    [
        value.wYear,
        value.wMonth,
        value.wDayOfWeek,
        value.wDay,
        value.wHour,
        value.wMinute,
        value.wSecond,
        value.wMilliseconds,
    ]
}

#[cfg(target_os = "windows")]
fn wide_string(value: &[u16]) -> String {
    let length = value
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..length])
}

#[cfg(target_os = "windows")]
impl TimeZoneSnapshot {
    fn display_name(&self) -> &str {
        if self.key_name.is_empty() {
            &self.standard_name
        } else {
            &self.key_name
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE,
        WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };

    #[test]
    fn windows_probe_reads_live_machine_state() {
        let probe = windows_probe();

        assert_eq!(probe.platform, "Windows");
        assert!(probe.idle_duration_ms.is_some());
        assert!(probe.display_count.unwrap_or_default() >= 1);
        assert!(probe.capabilities.len() >= 8);
        for key in [
            "clock-change-events",
            "timezone-change-events",
            "display-change-events",
        ] {
            assert!(probe
                .capabilities
                .iter()
                .any(|capability| capability.key == key));
        }
    }

    #[test]
    fn native_message_classification_preserves_power_and_session_events() {
        assert_eq!(
            lifecycle_event_kind(WM_POWERBROADCAST, PBT_APMSUSPEND),
            Some("sleep")
        );
        assert_eq!(
            lifecycle_event_kind(WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC),
            Some("wake")
        );
        assert_eq!(
            lifecycle_event_kind(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK),
            Some("lock")
        );
        assert_eq!(
            lifecycle_event_kind(WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK),
            Some("unlock")
        );
        assert_eq!(lifecycle_event_kind(0, 0), None);
    }

    #[test]
    fn display_message_dimensions_are_decoded_from_lparam() {
        let parameter = ((1440_u32 << 16) | 2560_u32) as isize;
        assert_eq!(display_dimensions(parameter), (2560, 1440));
    }

    #[test]
    fn time_zone_snapshot_compares_all_scheduling_relevant_fields() {
        let original = time_zone_snapshot("China Standard Time", -480);
        assert_eq!(original, original.clone());

        let mut changed = original.clone();
        changed.dynamic_daylight_time_disabled = true;
        assert_ne!(original, changed);
    }

    #[test]
    fn lifecycle_event_serializes_camel_case_timestamp_and_optional_detail() {
        let event = LifecycleEvent {
            kind: "display_changed",
            observed_at: "2026-07-14T08:00:00Z".to_string(),
            detail: Some(serde_json::json!({ "displayCount": 2 })),
        };

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "kind": "display_changed",
                "observedAt": "2026-07-14T08:00:00Z",
                "detail": { "displayCount": 2 },
            })
        );
    }

    fn time_zone_snapshot(key_name: &str, bias: i32) -> TimeZoneSnapshot {
        TimeZoneSnapshot {
            bias,
            standard_name: key_name.to_string(),
            standard_date: [0; 8],
            standard_bias: 0,
            daylight_name: String::new(),
            daylight_date: [0; 8],
            daylight_bias: 0,
            key_name: key_name.to_string(),
            dynamic_daylight_time_disabled: false,
        }
    }
}
