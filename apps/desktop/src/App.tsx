import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Bell,
  Check,
  CheckCircle2,
  ChevronDown,
  Clock3,
  Coffee,
  Database,
  Download,
  EyeOff,
  HardDrive,
  Languages,
  LoaderCircle,
  Moon,
  Pause,
  Pencil,
  Play,
  Plus,
  Power,
  RefreshCw,
  RotateCcw,
  Settings,
  Sparkles,
  Trash2,
  X,
} from "lucide-react";
import "./App.css";
import { I18nProvider, localeOptions, useI18n, type Locale } from "./i18n";
import { appInvoke as invoke, appListen as listen, isTauriRuntime } from "./runtime";
import { UpdateProvider, useAppUpdater } from "./updater";

interface StoredReminder {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  revision: number;
  createdAtUtc: number;
  ruleSummary?: string | null;
  rule?: ReminderRuleDetails | null;
  nextTriggerAt?: string | null;
}

interface ReminderRuleDetails {
  kind: "fixed" | "interval" | "oneShot" | string;
  timezone: string;
  weekdays: string[];
  times: string[];
  localDateTime?: string | null;
  intervalMinutes?: number | null;
  activeWindowStart?: string | null;
  activeWindowEnd?: string | null;
  excludedWindowStart?: string | null;
  excludedWindowEnd?: string | null;
  anchorLocalDateTime?: string | null;
}

interface StorageStatus {
  schemaVersion: number;
  healthy: boolean;
  reminderCount: number;
  databasePath: string;
}

interface ReminderSurfacePayload {
  title: string;
  body: string;
  occurrenceId: string;
  scheduledAt: string;
  preview: boolean;
}

interface AutostartStatusShape {
  available: boolean;
  enabled: boolean | null;
  error: string | null;
}

interface PauseStatusShape {
  isPaused: boolean;
  pausedUntil: string | null;
  activeSessionIds: string[];
}

interface OnboardingStatusShape {
  completed: boolean;
  needsSetup: boolean;
  hasReminders: boolean;
}

interface ReminderSettingsShape {
  appDisplayName: string;
  autoDismissSeconds: number;
  quietHours: {
    enabled: boolean;
    startLocal: string;
    endLocal: string;
    timezone?: string | null;
  };
}

type Tab = "reminders" | "settings";
type ReminderRuleKind = "fixed" | "interval" | "oneShot";
type RepeatMode = "workdays" | "daily";
type NotificationState = "checking" | "granted" | "denied" | "unavailable";

type ThemeStyle = "pulse" | "nocturne" | "studio";
type ThemeAccent = "mint" | "violet" | "coral" | "sky";
type ThemeBackground = "solid" | "mesh" | "grid";

interface ThemeSettingsShape {
  style: ThemeStyle;
  accent: ThemeAccent;
  background: ThemeBackground;
}

const THEME_STORAGE_KEY = "takefive.theme";
const defaultThemeSettings: ThemeSettingsShape = {
  style: "pulse",
  accent: "mint",
  background: "grid",
};

const styleOptions: Array<{ value: ThemeStyle; labelKey: "stylePulse" | "styleNocturne" | "styleStudio" }> = [
  { value: "pulse", labelKey: "stylePulse" },
  { value: "nocturne", labelKey: "styleNocturne" },
  { value: "studio", labelKey: "styleStudio" },
];
const accentOptions: Array<{ value: ThemeAccent; labelKey: "accentMint" | "accentViolet" | "accentCoral" | "accentSky" }> = [
  { value: "mint", labelKey: "accentMint" },
  { value: "violet", labelKey: "accentViolet" },
  { value: "coral", labelKey: "accentCoral" },
  { value: "sky", labelKey: "accentSky" },
];
const backgroundOptions: Array<{ value: ThemeBackground; labelKey: "backgroundSolid" | "backgroundMesh" | "backgroundGrid" }> = [
  { value: "solid", labelKey: "backgroundSolid" },
  { value: "mesh", labelKey: "backgroundMesh" },
  { value: "grid", labelKey: "backgroundGrid" },
];

function readThemeSettings(): ThemeSettingsShape {
  try {
    const value = JSON.parse(window.localStorage.getItem(THEME_STORAGE_KEY) ?? "null") as Partial<ThemeSettingsShape> | null;
    return {
      style: value?.style === "pulse" || value?.style === "nocturne" || value?.style === "studio"
        ? value.style
        : defaultThemeSettings.style,
      accent: value?.accent === "mint" || value?.accent === "violet" || value?.accent === "coral" || value?.accent === "sky"
        ? value.accent
        : defaultThemeSettings.accent,
      background: value?.background === "solid" || value?.background === "mesh" || value?.background === "grid"
        ? value.background
        : defaultThemeSettings.background,
    };
  } catch {
    return defaultThemeSettings;
  }
}

type ThemeContextValue = {
  settings: ThemeSettingsShape;
  setSettings: (next: Partial<ThemeSettingsShape>) => void;
};

const ThemeContext = createContext<ThemeContextValue | null>(null);

function ThemeProvider({ children }: { children: ReactNode }) {
  const [settings, setSettingsState] = useState<ThemeSettingsShape>(readThemeSettings);

  useEffect(() => {
    const root = document.documentElement;
    root.dataset.themeStyle = settings.style;
    root.dataset.themeAccent = settings.accent;
    root.dataset.themeBackground = settings.background;
    root.style.colorScheme = settings.style === "nocturne" ? "dark" : "light";
    window.localStorage.setItem(THEME_STORAGE_KEY, JSON.stringify(settings));
  }, [settings]);

  const value = useMemo<ThemeContextValue>(() => ({
    settings,
    setSettings: (next) => setSettingsState((current) => ({ ...current, ...next })),
  }), [settings]);

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

function useTheme() {
  const value = useContext(ThemeContext);
  if (!value) throw new Error("useTheme must be used inside ThemeProvider");
  return value;
}
interface ReminderDraft {
  name: string;
  timezone: string;
  kind: ReminderRuleKind;
  localTime: string;
  repeatMode: RepeatMode;
  oneShotLocalDateTime: string;
  intervalMinutes: string;
  anchorLocalDateTime: string;
}

const WORKDAYS = ["mon", "tue", "wed", "thu", "fri"];
const EVERY_DAY = [...WORKDAYS, "sat", "sun"];
const SURFACE_AUTO_DISMISS_MS = 7_000;
const DEFAULT_APP_DISPLAY_NAME = "摸个鱼 TakeFive";

function defaultReminderSettings(): ReminderSettingsShape {
  return {
    appDisplayName: DEFAULT_APP_DISPLAY_NAME,
    autoDismissSeconds: 7,
    quietHours: {
      enabled: true,
      startLocal: "12:00",
      endLocal: "13:30",
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    },
  };
}

function localDateTimeAfter(minutes: number) {
  const date = new Date(Date.now() + minutes * 60_000);
  date.setSeconds(0, 0);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function localDateTimeAt(localTime: string, timezone?: string) {
  const parts = Object.fromEntries(
    new Intl.DateTimeFormat("en-CA", {
      timeZone: timezone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    }).formatToParts(new Date()).map((part) => [part.type, part.value]),
  );
  return `${parts.year}-${parts.month}-${parts.day}T${localTime}`;
}

function createDraft(): ReminderDraft {
  const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || "Asia/Shanghai";
  return {
    name: "",
    timezone,
    kind: "fixed",
    localTime: "10:30",
    repeatMode: "workdays",
    oneShotLocalDateTime: localDateTimeAfter(30),
    intervalMinutes: "60",
    anchorLocalDateTime: localDateTimeAt("09:00", timezone),
  };
}

function createDraftFromReminder(reminder: StoredReminder): ReminderDraft {
  const fallback = createDraft();
  const rule = reminder.rule;
  if (!rule) return { ...fallback, name: reminder.name };
  const kind: ReminderRuleKind = rule.kind === "interval" || rule.kind === "oneShot"
    ? rule.kind
    : "fixed";
  return {
    ...fallback,
    name: reminder.name,
    timezone: rule.timezone || fallback.timezone,
    kind,
    localTime: rule.times[0] ?? fallback.localTime,
    repeatMode: rule.weekdays.length === 7 ? "daily" : "workdays",
    oneShotLocalDateTime: rule.localDateTime ?? fallback.oneShotLocalDateTime,
    intervalMinutes: String(rule.intervalMinutes ?? fallback.intervalMinutes),
    anchorLocalDateTime: rule.anchorLocalDateTime ?? fallback.anchorLocalDateTime,
  };
}

function isValidTimeZone(timezone: string) {
  try {
    new Intl.DateTimeFormat("en", { timeZone: timezone }).format();
    return true;
  } catch {
    return false;
  }
}

function zonedDateTimeToTimestamp(value: string, timezone: string) {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(value);
  if (!match) return null;

  const desired = {
    year: Number(match[1]),
    month: Number(match[2]),
    day: Number(match[3]),
    hour: Number(match[4]),
    minute: Number(match[5]),
  };
  const desiredAsUtc = Date.UTC(
    desired.year,
    desired.month - 1,
    desired.day,
    desired.hour,
    desired.minute,
  );
  const normalized = new Date(desiredAsUtc);
  if (
    normalized.getUTCFullYear() !== desired.year
    || normalized.getUTCMonth() + 1 !== desired.month
    || normalized.getUTCDate() !== desired.day
    || normalized.getUTCHours() !== desired.hour
    || normalized.getUTCMinutes() !== desired.minute
  ) return null;

  const formatter = new Intl.DateTimeFormat("en-CA", {
    timeZone: timezone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  });
  let candidate = desiredAsUtc;
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const parts = Object.fromEntries(
      formatter.formatToParts(new Date(candidate)).map((part) => [part.type, part.value]),
    );
    const renderedAsUtc = Date.UTC(
      Number(parts.year),
      Number(parts.month) - 1,
      Number(parts.day),
      Number(parts.hour),
      Number(parts.minute),
    );
    const adjustment = desiredAsUtc - renderedAsUtc;
    if (adjustment === 0) return candidate;
    candidate += adjustment;
  }
  return null;
}

function formatDateTime(
  value: string | null | undefined,
  locale: Locale,
  fallback: string,
  timeZone?: string,
) {
  if (!value) return fallback;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(locale, {
    month: "numeric",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
    ...(timeZone ? { timeZone } : {}),
  });
}

function readableError(error: unknown, t: ReturnType<typeof useI18n>["t"]) {
  const message = String(error).replace(/^Error:\s*/, "");
  const known: Record<string, string> = {
    reminder_revision_conflict: t("revisionConflict"),
    occurrence_not_found: t("occurrenceMissing"),
    occurrence_action_conflict: t("occurrenceConflict"),
    reminder_not_found: t("occurrenceMissing"),
    autostart_verification_failed: t("systemUnavailable"),
    reminder_surface_failed: t("surfaceError"),
    system_notification_failed: t("notificationPermissionOff"),
  };
  if (known[message]) return known[message];
  if (message.includes("reminder revision conflict")) return t("revisionConflict");
  if (message === "app_display_name_out_of_range") return t("appDisplayNameRange");
  if (message === "auto_dismiss_seconds_out_of_range") return t("autoDismissRange");
  if (message === "quiet_hours_start_equals_end") return t("quietHoursSameTime");
  if (message === "invalid_quiet_hours_time") return t("invalidLocalTime");
  if (message === "invalid_quiet_hours_timezone") return t("invalidTimezone");
  if (message.startsWith("无法识别时区") || message === "invalid_timezone") return t("invalidTimezone");
  if (message.includes("提醒时间格式") || message.includes("时间格式应为 HH:mm")) return t("invalidLocalTime");
  if (message.includes("一次性提醒时间必须晚于当前时间")) return t("oneShotPast");
  if (message.includes("一次性提醒时间格式")) return t("invalidLocalTime");
  if (message.includes("间隔时长必须") || message.includes("暂停时长必须")) return t("intervalRange");
  if (message.includes("锚点时间格式")) return t("invalidAnchor");
  if (message.includes("提醒名称需为")) return t("reminderNameLength");
  if (message.includes("午休时间必须")) return t("lunchOutsideWindow");
  if (message.includes("活动窗口开始和结束时间")) return t("windowSame");
  return message.replace(/_/g, " ");
}

function TeaLogo({ className = "" }: { className?: string }) {
  return (
    <span className={`tea-logo ${className}`} aria-hidden="true">
      <svg width="22" height="22" viewBox="10 8 48 48" fill="none">
        <rect x="21" y="11" width="6" height="13" rx="3" fill="#E7B755" />
        <rect x="34" y="11" width="6" height="13" rx="3" fill="#E7B755" />
        <path d="M14 29h32v12c0 7.18-5.82 13-13 13h-6c-7.18 0-13-5.82-13-13V29Z" fill="currentColor" />
        <path d="M45 33h4a7 7 0 0 1 0 14h-4" stroke="currentColor" strokeWidth="5" strokeLinecap="round" />
      </svg>
    </span>
  );
}

function localizeSurfaceBody(body: string, t: ReturnType<typeof useI18n>["t"]) {
  return body === "到时间啦，别忘了照顾一下自己。" ? t("notificationReady") : body;
}

function normalizeAutostart(value: AutostartStatusShape | undefined) {
  return {
    enabled: value?.enabled ?? false,
    supported: value?.available ?? true,
    error: value?.error ?? null,
  };
}

function normalizePause(value: PauseStatusShape | null | undefined) {
  return {
    active: value?.isPaused ?? false,
    endsAt: value?.pausedUntil ?? null,
  };
}

const weekdayTranslationKeys = {
  mon: "dayMon",
  tue: "dayTue",
  wed: "dayWed",
  thu: "dayThu",
  fri: "dayFri",
  sat: "daySat",
  sun: "daySun",
} as const;

function formatWeekdays(weekdays: string[], locale: Locale, t: ReturnType<typeof useI18n>["t"]) {
  const normalized = weekdays.map((day) => day.toLowerCase()).filter((day) => day in weekdayTranslationKeys);
  if (normalized.length === 7) return t("everyDay");
  if (normalized.length === 5 && ["mon", "tue", "wed", "thu", "fri"].every((day) => normalized.includes(day))) {
    return t("workdays");
  }
  const labels = normalized.map((day) => t(weekdayTranslationKeys[day as keyof typeof weekdayTranslationKeys]));
  if (labels.length < 2) return labels[0] ?? t("ruleUnavailable");
  const conjunction = locale === "en-US" ? " and " : locale === "es-ES" ? " y " : locale === "ja-JP" ? "・" : "、";
  return `${labels.slice(0, -1).join(locale === "en-US" || locale === "es-ES" ? ", " : "、")}${conjunction}${labels[labels.length - 1]}`;
}

function formatRuleLocalDateTime(value: string | null | undefined, timezone: string, locale: Locale, fallback: string) {
  if (!value) return fallback;
  const timestamp = zonedDateTimeToTimestamp(value, timezone);
  if (timestamp === null) return value.replace("T", " ");
  return formatDateTime(new Date(timestamp).toISOString(), locale, value, timezone);
}

function formatRuleSummary(
  rule: ReminderRuleDetails | null | undefined,
  locale: Locale,
  t: ReturnType<typeof useI18n>["t"],
) {
  if (!rule) return t("ruleUnavailable");
  const weekdays = formatWeekdays(rule.weekdays, locale, t);
  if (rule.kind === "fixed") {
    return `${weekdays} · ${rule.times.join(", ")}`;
  }
  if (rule.kind === "oneShot") {
    return `${t("oneShot")} · ${formatRuleLocalDateTime(rule.localDateTime, rule.timezone, locale, t("waitingCalculation"))}`;
  }

  const interval = t("everyMinutes", { value: rule.intervalMinutes ?? "?" });
  let window = t("activeAllDay");
  if (rule.activeWindowStart && rule.activeWindowEnd) {
    window = t("activeWindowSummary", {
      start: rule.activeWindowStart,
      end: rule.activeWindowEnd,
    });
  }
  return `${weekdays} · ${interval} · ${window}`;
}

function ReminderSurface() {
  const { locale, t } = useI18n();
  const [payload, setPayload] = useState<ReminderSurfacePayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [dismissing, setDismissing] = useState(false);
  const [error, setError] = useState("");
  const [autoDismissMs, setAutoDismissMs] = useState(SURFACE_AUTO_DISMISS_MS);
  const [appDisplayName, setAppDisplayName] = useState(DEFAULT_APP_DISPLAY_NAME);
  const dismissingId = useRef<string | null>(null);

  const performAction = useCallback(async (
    command: "mark_occurrence_unhandled" | "dismiss_reminder_preview",
    occurrenceId: string,
  ) => {
    if (dismissingId.current === occurrenceId) return;
    dismissingId.current = occurrenceId;
    setDismissing(true);
    setError("");

    await new Promise((resolve) => window.setTimeout(resolve, 100));
    try {
      await invoke(command, { id: occurrenceId });
    } catch (reason) {
      const message = String(reason);
      if (!message.includes("occurrence_action_conflict") && !message.includes("occurrence_not_found")) {
        setError(t("actionFailed"));
        setDismissing(false);
      }
      dismissingId.current = null;
    }
  }, [t]);

  const dismiss = useCallback((current: ReminderSurfacePayload) => (
    performAction(
      current.preview ? "dismiss_reminder_preview" : "mark_occurrence_unhandled",
      current.occurrenceId,
    )
  ), [performAction]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
    const refresh = () => invoke<ReminderSettingsShape>("get_reminder_settings", { timezone })
      .then((settings) => {
        if (!disposed) {
          setAutoDismissMs(settings.autoDismissSeconds * 1_000);
          setAppDisplayName(settings.appDisplayName || DEFAULT_APP_DISPLAY_NAME);
        }
      })
      .catch(() => {
        if (!disposed) {
          setAutoDismissMs(SURFACE_AUTO_DISMISS_MS);
          setAppDisplayName(DEFAULT_APP_DISPLAY_NAME);
        }
      });
    void refresh();
    void listen("settings-changed", () => {
      void refresh();
    }).then((disposeListener) => {
      if (disposed) disposeListener();
      else unlisten = disposeListener;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    document.documentElement.classList.add("surface-document");
    let disposed = false;
    let unlisten: (() => void) | undefined;

    const refresh = async () => {
      try {
        const next = await invoke<ReminderSurfacePayload | null>("get_reminder_surface_payload");
        if (!disposed) {
          setPayload(next);
          setError(next ? "" : t("contentUnavailable"));
        }
      } catch (reason) {
        if (!disposed) setError(readableError(reason, t));
      } finally {
        if (!disposed) setLoading(false);
      }
    };

    void refresh();
    void listen<ReminderSurfacePayload>("reminder-surface-updated", (event) => {
      if (!disposed) {
        setPayload(event.payload);
        dismissingId.current = null;
        setDismissing(false);
        setError("");
        setLoading(false);
      }
    }).then((disposeListener) => {
      if (disposed) disposeListener();
      else unlisten = disposeListener;
    }).catch((reason) => {
      if (!disposed) setError(readableError(reason, t));
    });

    return () => {
      disposed = true;
      unlisten?.();
      document.documentElement.classList.remove("surface-document");
    };
  }, [t]);

  useEffect(() => {
    if (!payload) return undefined;
    const timer = window.setTimeout(() => {
      void dismiss(payload);
    }, autoDismissMs);

    return () => {
      window.clearTimeout(timer);
    };
  }, [autoDismissMs, dismiss, payload]);

  return (
    <main
      key={payload?.occurrenceId ?? "surface-loading"}
      className={`surface-shell ${dismissing ? "leaving" : ""}`}
      role={payload ? "dialog" : "status"}
      aria-live="polite"
      aria-atomic="true"
      aria-labelledby={payload ? "surface-title" : undefined}
    >
      <span className="surface-mascot" aria-hidden="true">
        <Coffee size={23} strokeWidth={2.2} />
        <Sparkles className="surface-spark" size={12} />
      </span>

      {payload ? (
        <section className="surface-message" aria-labelledby="surface-title">
          <button
            className="surface-dismiss"
            type="button"
            title={t("dismiss")}
            aria-label={t("dismiss")}
            disabled={dismissing}
            onClick={() => void dismiss(payload)}
          >
            <X size={15} />
          </button>
          <div className="surface-meta">
            <span>{payload.preview ? `${appDisplayName} · ${t("previewLabel")}` : appDisplayName}</span>
            <time>{formatDateTime(payload.scheduledAt, locale, t("waitingCalculation"))}</time>
          </div>
          <h1 id="surface-title">{payload.title}</h1>
          <p>{error || localizeSurfaceBody(payload.body, t)}</p>
        </section>
      ) : (
        <div className="surface-loading">
          {loading && <LoaderCircle className="spin" size={16} />}
          <span>{loading ? t("loadingReminder") : error}</span>
        </div>
      )}
      {payload && (
        <span
          className="surface-progress"
          aria-hidden="true"
          style={{ animationDuration: `${autoDismissMs}ms` }}
        />
      )}
    </main>
  );
}

function Toggle({
  checked,
  disabled,
  label,
  onChange,
}: {
  checked: boolean;
  disabled?: boolean;
  label: string;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="toggle" title={label}>
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
      />
      <span aria-hidden="true" />
      <span className="sr-only">{label}</span>
    </label>
  );
}

function ReminderEditor({
  reminder,
  onClose,
  onSaved,
}: {
  reminder: StoredReminder | null;
  onClose: () => void;
  onSaved: () => Promise<void>;
}) {
  const { t } = useI18n();
  const [draft, setDraft] = useState(() => reminder ? createDraftFromReminder(reminder) : createDraft());
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const isEditing = reminder !== null;

  const update = <K extends keyof ReminderDraft>(key: K, value: ReminderDraft[K]) => {
    setDraft((current) => ({ ...current, [key]: value }));
  };

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setError("");
    const name = draft.name.trim();
    const timezone = draft.timezone.trim();
    if (!name || name.length > 30) {
      setError(t("reminderNameLength"));
      return;
    }
    if (!isValidTimeZone(timezone)) {
      setError(t("invalidTimezone"));
      return;
    }

    let command: string;
    let input: Record<string, unknown>;
    if (draft.kind === "fixed") {
      command = "create_reminder";
      input = {
        name,
        description: null,
        localTime: draft.localTime,
        timezone,
        weekdays: draft.repeatMode === "workdays" ? WORKDAYS : EVERY_DAY,
      };
    } else if (draft.kind === "oneShot") {
      const scheduledAt = zonedDateTimeToTimestamp(draft.oneShotLocalDateTime, timezone);
      if (scheduledAt === null) {
        setError(t("invalidLocalTime"));
        return;
      }
      if (scheduledAt <= Date.now()) {
        setError(t("oneShotPast"));
        return;
      }
      command = "create_one_shot_reminder";
      input = {
        name,
        description: null,
        localDateTime: draft.oneShotLocalDateTime,
        timezone,
      };
    } else {
      const intervalMinutes = Number(draft.intervalMinutes);
      const intervalAnchor = reminder?.rule?.kind === "interval" && reminder.rule.anchorLocalDateTime
        ? reminder.rule.anchorLocalDateTime
        : localDateTimeAt(localDateTimeAfter(0).slice(11), timezone);
      if (!Number.isInteger(intervalMinutes) || intervalMinutes < 1 || intervalMinutes > 1440) {
        setError(t("intervalRange"));
        return;
      }
      if (zonedDateTimeToTimestamp(intervalAnchor, timezone) === null) {
        setError(t("invalidAnchor"));
        return;
      }
      command = "create_aligned_interval_reminder";
      input = {
        name,
        description: reminder?.description || null,
        intervalMinutes,
        anchorLocalDateTime: intervalAnchor,
        timezone,
        weekdays: draft.repeatMode === "workdays" ? WORKDAYS : EVERY_DAY,
        activeWindowStart: null,
        activeWindowEnd: null,
        excludedWindowStart: null,
        excludedWindowEnd: null,
      };
    }

    setSaving(true);
    try {
      await invoke(isEditing ? "update_reminder" : command, {
        input: isEditing
          ? {
            ...input,
            id: reminder.id,
            expectedRevision: reminder.revision,
            kind: draft.kind,
            description: reminder.description || null,
          }
          : input,
      });
      await onSaved();
      onClose();
    } catch (reason) {
      setError(readableError(reason, t));
    } finally {
      setSaving(false);
    }
  };

  return (
    <aside className="editor-pane" aria-labelledby="editor-title">
      <div className="editor-heading">
        <div>
          <span className="eyebrow">{isEditing ? t("editingReminder") : t("newReminder")}</span>
          <h2 id="editor-title">{isEditing ? reminder.name : t("scheduleTime")}</h2>
        </div>
        <button className="icon-button" type="button" title={t("close")} onClick={onClose}>
          <X size={18} />
        </button>
      </div>

      <form className="reminder-form" onSubmit={(event) => void submit(event)}>
        <label className="field full-span">
          <span>{t("name")}</span>
          <input
            autoFocus
            value={draft.name}
            maxLength={30}
            placeholder={t("namePlaceholder")}
            onChange={(event) => update("name", event.target.value)}
          />
        </label>

        <fieldset className="field full-span">
          <legend>{t("reminderMethod")}</legend>
          <div className="segmented three">
            <label>
              <input type="radio" name="kind" checked={draft.kind === "fixed"} onChange={() => update("kind", "fixed")} />
              <span>{t("fixedTime")}</span>
            </label>
            <label>
              <input type="radio" name="kind" checked={draft.kind === "interval"} onChange={() => update("kind", "interval")} />
              <span>{t("intervalLoop")}</span>
            </label>
            <label>
              <input type="radio" name="kind" checked={draft.kind === "oneShot"} onChange={() => update("kind", "oneShot")} />
              <span>{t("oneShot")}</span>
            </label>
          </div>
        </fieldset>

        {draft.kind === "fixed" && (
          <>
            <label className="field">
              <span>{t("time")}</span>
              <input type="time" required value={draft.localTime} onChange={(event) => update("localTime", event.target.value)} />
            </label>
            <fieldset className="field">
              <legend>{t("repeat")}</legend>
              <div className="segmented">
                <label>
                  <input type="radio" name="repeat" checked={draft.repeatMode === "workdays"} onChange={() => update("repeatMode", "workdays")} />
                  <span>{t("workdays")}</span>
                </label>
                <label>
                  <input type="radio" name="repeat" checked={draft.repeatMode === "daily"} onChange={() => update("repeatMode", "daily")} />
                  <span>{t("everyDay")}</span>
                </label>
              </div>
            </fieldset>
          </>
        )}

        {draft.kind === "oneShot" && (
          <label className="field full-span">
            <span>{t("dateAndTime")}</span>
            <input
              type="datetime-local"
              required
              value={draft.oneShotLocalDateTime}
              onChange={(event) => update("oneShotLocalDateTime", event.target.value)}
            />
          </label>
        )}

        {draft.kind === "interval" && (
          <>
            <label className="field full-span">
              <span>{t("intervalMinutes")}</span>
              <input
                type="number"
                min={1}
                max={1440}
                step={1}
                required
                value={draft.intervalMinutes}
                onChange={(event) => update("intervalMinutes", event.target.value)}
              />
            </label>
            <fieldset className="field full-span">
              <legend>{t("repeat")}</legend>
              <div className="segmented">
                <label>
                  <input type="radio" name="interval-repeat" checked={draft.repeatMode === "workdays"} onChange={() => update("repeatMode", "workdays")} />
                  <span>{t("workdaysOnly")}</span>
                </label>
                <label>
                  <input type="radio" name="interval-repeat" checked={draft.repeatMode === "daily"} onChange={() => update("repeatMode", "daily")} />
                  <span>{t("everyDay")}</span>
                </label>
              </div>
            </fieldset>
          </>
        )}

        <label className="field full-span">
          <span>{t("timezone")}</span>
          <input value={draft.timezone} onChange={(event) => update("timezone", event.target.value)} spellCheck={false} />
        </label>

        {error && <p className="form-error full-span" role="alert">{error}</p>}

        <div className="form-actions full-span">
          <button className="button ghost" type="button" onClick={onClose}>{t("cancel")}</button>
          <button className="button primary" type="submit" disabled={saving}>
            {saving ? <LoaderCircle className="spin" size={17} /> : <Check size={17} />}
            {isEditing ? t("saveChanges") : t("saveReminder")}
          </button>
        </div>
      </form>
    </aside>
  );
}

function RemindersPage() {
  const { locale, t } = useI18n();
  const queryClient = useQueryClient();
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingReminder, setEditingReminder] = useState<StoredReminder | null>(null);
  const [rowError, setRowError] = useState("");
  const remindersQuery = useQuery({
    queryKey: ["reminders"],
    queryFn: () => invoke<StoredReminder[]>("list_reminders"),
    retry: false,
  });

  const refresh = () => queryClient.invalidateQueries({ queryKey: ["reminders"] });
  const setEnabled = useMutation({
    mutationFn: ({ reminder, enabled }: { reminder: StoredReminder; enabled: boolean }) =>
      invoke("set_reminder_enabled", {
        id: reminder.id,
        expectedRevision: reminder.revision,
        enabled,
      }),
    onSuccess: async () => {
      setRowError("");
      await refresh();
    },
    onError: (reason) => setRowError(readableError(reason, t)),
  });
  const deleteReminder = useMutation({
    mutationFn: (id: string) => invoke<boolean>("delete_reminder", { id }),
    onSuccess: async () => {
      setRowError("");
      await refresh();
    },
    onError: (reason) => setRowError(readableError(reason, t)),
  });
  const previewReminder = useMutation({
    mutationFn: (id: string) => invoke<ReminderSurfacePayload>("preview_reminder", { id }),
    onSuccess: () => setRowError(""),
    onError: (reason) => setRowError(readableError(reason, t)),
  });

  const openNewReminder = () => {
    setEditingReminder(null);
    setEditorOpen(true);
  };
  const openReminderEditor = (reminder: StoredReminder) => {
    setEditingReminder(reminder);
    setEditorOpen(true);
  };
  const closeEditor = () => {
    setEditorOpen(false);
    setEditingReminder(null);
  };

  return (
    <div className={`reminders-layout ${editorOpen ? "with-editor" : ""}`}>
      <section className="page-pane" aria-labelledby="reminders-title">
        <div className="page-heading">
          <div>
            <span className="eyebrow">{t("reminderCount", { count: remindersQuery.data?.length ?? 0 })}</span>
            <h1 id="reminders-title">{t("navReminders")}</h1>
          </div>
          <button className="button primary" type="button" onClick={openNewReminder}>
            <Plus size={18} />{t("newReminder")}
          </button>
        </div>

        {rowError && <p className="page-error" role="alert">{rowError}</p>}
        {remindersQuery.isError && (
          <div className="load-state" role="alert">
            <Bell size={26} />
            <strong>{t("remindersReadFailed")}</strong>
            <span>{readableError(remindersQuery.error, t)}</span>
            <button className="button secondary" type="button" onClick={() => void remindersQuery.refetch()}>
              <RotateCcw size={16} />{t("retry")}
            </button>
          </div>
        )}
        {remindersQuery.isLoading && (
          <div className="load-state"><LoaderCircle className="spin" size={24} /><span>{t("loading")}</span></div>
        )}
        {remindersQuery.isSuccess && remindersQuery.data.length === 0 && (
          <div className="empty-state">
            <span className="empty-icon"><Bell size={28} /></span>
            <h2>{t("noReminders")}</h2>
            <button className="button secondary" type="button" onClick={openNewReminder}>
              <Plus size={17} />{t("newReminder")}
            </button>
          </div>
        )}
        {remindersQuery.isSuccess && remindersQuery.data.length > 0 && (
          <div className="reminder-list" aria-label={t("reminderList")}>
            {remindersQuery.data.map((reminder) => {
              const toggling = setEnabled.isPending && setEnabled.variables?.reminder.id === reminder.id;
              const deleting = deleteReminder.isPending && deleteReminder.variables === reminder.id;
              const previewing = previewReminder.isPending && previewReminder.variables === reminder.id;
              return (
                <article className={`reminder-row ${reminder.enabled ? "" : "disabled"}`} key={reminder.id}>
                  <span className="rule-icon" aria-hidden="true">
                    {reminder.rule?.kind === "interval"
                      ? <RotateCcw size={18} />
                      : <Clock3 size={18} />}
                  </span>
                  <div className="reminder-copy">
                    <strong>{reminder.name}</strong>
                    <span>{reminder.rule ? formatRuleSummary(reminder.rule, locale, t) : t("ruleLoading")}</span>
                  </div>
                  <div className="next-time">
                    <span>{t("next")}</span>
                    <strong>{reminder.enabled ? formatDateTime(reminder.nextTriggerAt, locale, t("waitingCalculation")) : t("disabled")}</strong>
                  </div>
                  <div className="row-actions">
                    <button
                      className="icon-button edit"
                      type="button"
                      title={t("editReminder", { name: reminder.name })}
                      aria-label={t("editReminder", { name: reminder.name })}
                      disabled={previewReminder.isPending || deleteReminder.isPending || setEnabled.isPending}
                      onClick={() => openReminderEditor(reminder)}
                    >
                      <Pencil size={16} />
                    </button>
                    <button
                      className="icon-button preview"
                      type="button"
                      title={t("previewReminder", { name: reminder.name })}
                      aria-label={t("previewReminder", { name: reminder.name })}
                      disabled={previewReminder.isPending || deleteReminder.isPending || setEnabled.isPending}
                      onClick={() => previewReminder.mutate(reminder.id)}
                    >
                      {previewing ? <LoaderCircle className="spin" size={17} /> : <Play size={17} />}
                    </button>
                    {toggling
                      ? <LoaderCircle className="spin row-loader" size={17} />
                      : (
                        <Toggle
                          checked={reminder.enabled}
                          disabled={setEnabled.isPending || deleteReminder.isPending || previewReminder.isPending}
                          label={reminder.enabled ? t("disable", { name: reminder.name }) : t("enable", { name: reminder.name })}
                          onChange={(enabled) => setEnabled.mutate({ reminder, enabled })}
                        />
                      )}
                    <button
                      className="icon-button danger"
                      type="button"
                      title={t("delete", { name: reminder.name })}
                      disabled={deleteReminder.isPending || setEnabled.isPending || previewReminder.isPending}
                      onClick={() => deleteReminder.mutate(reminder.id)}
                    >
                      {deleting ? <LoaderCircle className="spin" size={17} /> : <Trash2 size={17} />}
                    </button>
                  </div>
                </article>
              );
            })}
          </div>
        )}
      </section>

      {editorOpen && (
        <ReminderEditor
          key={editingReminder?.id ?? "new-reminder"}
          reminder={editingReminder}
          onClose={closeEditor}
          onSaved={async () => {
            await Promise.all([
              refresh(),
              queryClient.invalidateQueries({ queryKey: ["storage-status"] }),
            ]);
          }}
        />
      )}
    </div>
  );
}

function SettingsPage() {
  const { locale, setLocale, t } = useI18n();
  const { settings: themeSettings, setSettings: setThemeSettings } = useTheme();
  const updater = useAppUpdater();
  const queryClient = useQueryClient();
  const [notificationState, setNotificationState] = useState<NotificationState>("checking");
  const [testingNotification, setTestingNotification] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [reminderSettings, setReminderSettings] = useState(defaultReminderSettings);
  const reminderSettingsDraft = useRef(defaultReminderSettings());
  const savedReminderSettings = useRef(defaultReminderSettings());
  const reminderSettingsSaveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const reminderSettingsSaveInFlight = useRef(false);

  const autostartQuery = useQuery({
    queryKey: ["autostart"],
    queryFn: () => invoke<AutostartStatusShape>("get_autostart_status"),
    retry: false,
  });
  const pauseQuery = useQuery({
    queryKey: ["pause-status"],
    queryFn: () => invoke<PauseStatusShape | null>("get_pause_status"),
    refetchInterval: 30_000,
    retry: false,
  });
  const storageQuery = useQuery({
    queryKey: ["storage-status"],
    queryFn: () => invoke<StorageStatus>("storage_status"),
    retry: false,
  });
  const reminderSettingsQuery = useQuery({
    queryKey: ["reminder-settings"],
    queryFn: () => invoke<ReminderSettingsShape>("get_reminder_settings", {
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    }),
    retry: false,
  });

  const autostart = normalizeAutostart(autostartQuery.data);
  const pause = normalizePause(pauseQuery.data);
  const updateBusy = ["checking", "downloading", "installing", "restarting"].includes(updater.phase);
  const updateStatus = !updater.supported
    ? t("updatesInstalledOnly")
    : updater.phase === "checking"
      ? t("checkingForUpdates")
      : updater.phase === "upToDate"
        ? t("upToDate")
        : updater.phase === "available" && updater.availableUpdate
          ? t("updateAvailable", { version: updater.availableUpdate.version })
          : updater.phase === "downloading"
            ? updater.progressPercent === null
              ? t("downloadingUpdate")
              : t("updateProgress", { progress: updater.progressPercent })
            : updater.phase === "installing"
              ? t("installingUpdate")
              : updater.phase === "restarting"
                ? t("restartingUpdate")
                : t("updatesDescription");

  useEffect(() => {
    if (reminderSettingsQuery.data) {
      reminderSettingsDraft.current = reminderSettingsQuery.data;
      savedReminderSettings.current = reminderSettingsQuery.data;
      setReminderSettings(reminderSettingsQuery.data);
    }
  }, [reminderSettingsQuery.data]);

  useEffect(() => () => {
    if (reminderSettingsSaveTimer.current) clearTimeout(reminderSettingsSaveTimer.current);
  }, []);

  useEffect(() => {
    if (!isTauriRuntime) {
      setNotificationState("unavailable");
      return;
    }
    isPermissionGranted()
      .then((granted) => setNotificationState(granted ? "granted" : "denied"))
      .catch(() => setNotificationState("unavailable"));
  }, []);

  const setAutostart = useMutation({
    mutationFn: (enabled: boolean) => invoke("set_autostart_enabled", { enabled }),
    onSuccess: async () => {
      setError("");
      setMessage(t("autostartUpdated"));
      await queryClient.invalidateQueries({ queryKey: ["autostart"] });
    },
    onError: (reason) => setError(readableError(reason, t)),
  });
  const changePause = useMutation({
    mutationFn: ({ action, minutes }: { action: "pause" | "resume"; minutes?: number }) =>
      action === "pause" ? invoke("pause_all", { minutes }) : invoke("resume_all"),
    onSuccess: async (_, variables) => {
      setError("");
      setMessage(variables.action === "pause" ? t("remindersPaused") : t("remindersResumed"));
      await queryClient.invalidateQueries({ queryKey: ["pause-status"] });
    },
    onError: (reason) => setError(readableError(reason, t)),
  });
  const saveReminderSettings = useMutation({
    mutationFn: (settings: ReminderSettingsShape) => invoke<ReminderSettingsShape>(
      "update_reminder_settings",
      { input: settings },
    ),
    onSuccess: async (settings) => {
      reminderSettingsSaveInFlight.current = false;
      reminderSettingsSaveTimer.current = null;
      setError("");
      setMessage(t("reminderSettingsSaved"));
      reminderSettingsDraft.current = settings;
      savedReminderSettings.current = settings;
      setReminderSettings(settings);
      await queryClient.invalidateQueries({ queryKey: ["reminder-settings"] });
    },
    onError: (reason) => {
      reminderSettingsSaveInFlight.current = false;
      reminderSettingsSaveTimer.current = null;
      reminderSettingsDraft.current = savedReminderSettings.current;
      setReminderSettings(savedReminderSettings.current);
      setError(readableError(reason, t));
    },
  });

  const stageReminderSettings = (settings: ReminderSettingsShape) => {
    reminderSettingsDraft.current = settings;
    setReminderSettings(settings);
  };

  const reminderSettingsValidationError = (settings: ReminderSettingsShape) => {
    const displayNameLength = [...settings.appDisplayName.trim()].length;
    if (displayNameLength < 1 || displayNameLength > 30) {
      return t("appDisplayNameRange");
    }
    if (
      settings.autoDismissSeconds < 1
      || settings.autoDismissSeconds > 60
    ) {
      return t("autoDismissRange");
    }
    if (settings.quietHours.startLocal === settings.quietHours.endLocal) {
      return t("quietHoursSameTime");
    }
    return "";
  };

  const persistReminderSettings = (settings: ReminderSettingsShape) => {
    stageReminderSettings(settings);
    setMessage("");
    const validationError = reminderSettingsValidationError(settings);
    setError(validationError);
    if (validationError) {
      return;
    }
    if (reminderSettingsSaveInFlight.current) return;
    reminderSettingsSaveInFlight.current = true;
    saveReminderSettings.mutate({
      ...settings,
      quietHours: {
        ...settings.quietHours,
        timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
      },
    });
  };

  const applyReminderSettings = (settings: ReminderSettingsShape) => {
    if (reminderSettingsSaveTimer.current) {
      clearTimeout(reminderSettingsSaveTimer.current);
      reminderSettingsSaveTimer.current = null;
    }
    persistReminderSettings(settings);
  };

  const scheduleReminderSettingsSave = (settings: ReminderSettingsShape) => {
    stageReminderSettings(settings);
    setMessage("");
    const validationError = reminderSettingsValidationError(settings);
    setError(validationError);
    if (reminderSettingsSaveTimer.current) clearTimeout(reminderSettingsSaveTimer.current);
    if (validationError) {
      reminderSettingsSaveTimer.current = null;
      return;
    }
    reminderSettingsSaveTimer.current = setTimeout(() => {
      reminderSettingsSaveTimer.current = null;
      persistReminderSettings(settings);
    }, 400);
  };

  const updateTheme = (next: Partial<ThemeSettingsShape>) => {
    setThemeSettings(next);
    setError("");
    setMessage(t("themeSaved"));
  };

  const enableNotifications = async () => {
    setError("");
    if (!isTauriRuntime) {
      setNotificationState("unavailable");
      setError(t("systemUnavailable"));
      return;
    }
    try {
      const permission = await requestPermission();
      const granted = permission === "granted";
      setNotificationState(granted ? "granted" : "denied");
      if (!granted) setError(t("notificationPermissionOff"));
    } catch (reason) {
      setNotificationState("unavailable");
      setError(readableError(reason, t));
    }
  };

  const testNotification = async () => {
    setTestingNotification(true);
    setError("");
    setMessage("");
    if (!isTauriRuntime) {
      setTestingNotification(false);
      setError(t("systemUnavailable"));
      return;
    }
    try {
      let granted = await isPermissionGranted();
      if (!granted) {
        granted = (await requestPermission()) === "granted";
        setNotificationState(granted ? "granted" : "denied");
      }
      if (!granted) throw new Error(t("notificationPermissionOff"));
      sendNotification({
        title: reminderSettingsDraft.current.appDisplayName,
        body: t("notificationReady"),
      });
      setMessage(t("testNotificationSent"));
    } catch (reason) {
      setError(readableError(reason, t));
    } finally {
      setTestingNotification(false);
    }
  };

  return (
    <section className="settings-page" aria-labelledby="settings-title">
      <div className="page-heading settings-heading">
        <div>
          <span className="eyebrow">{t("preference")}</span>
          <h1 id="settings-title">{t("navSettings")}</h1>
        </div>
      </div>

      {(error || message) && (
        <p className={error ? "page-error" : "page-message"} role={error ? "alert" : "status"}>
          {error || message}
        </p>
      )}

      <div className="settings-section reminder-control-section">
        <h2>{t("notifications")}</h2>
        <div className="setting-row pause-row">
          <span className={`setting-icon ${pause.active ? "paused" : ""}`}><Pause size={19} /></span>
          <div className="setting-copy">
            <strong>{t("allReminders")}</strong>
            <span>{t("allRemindersDescription")}</span>
            <span>{pause.active ? t("pausedUntil", { time: formatDateTime(pause.endsAt, locale, t("waitingCalculation")) }) : t("runningNormally")}</span>
          </div>
          <div className="setting-actions pause-actions">
            {pause.active ? (
              <button
                className="button primary compact"
                type="button"
                disabled={changePause.isPending}
                onClick={() => changePause.mutate({ action: "resume" })}
              >
                <Play size={15} />{t("resumeNow")}
              </button>
            ) : (
              [30, 60, 120].map((minutes) => (
                <button
                  className="button secondary compact"
                  type="button"
                  key={minutes}
                  disabled={changePause.isPending}
                  onClick={() => changePause.mutate({ action: "pause", minutes })}
                >
                  {minutes < 60 ? t("minutes", { value: minutes }) : t("hours", { value: minutes / 60 })}
                </button>
              ))
            )}
          </div>
        </div>
      </div>

      <div className="settings-section reminder-behavior-section">
        <h2>{t("reminderBehavior")}</h2>
        <div className="setting-row">
          <span className="setting-icon"><Clock3 size={19} /></span>
          <div className="setting-copy">
            <strong>{t("autoDismiss")}</strong>
            <span>{t("autoDismissDescription")}</span>
          </div>
          <label className="duration-setting">
            <input
              type="number"
              min={1}
              max={60}
              step={1}
              required
              disabled={reminderSettingsQuery.isLoading || saveReminderSettings.isPending}
              value={reminderSettings.autoDismissSeconds}
              aria-label={t("autoDismiss")}
              onChange={(event) => scheduleReminderSettingsSave({
                ...reminderSettingsDraft.current,
                autoDismissSeconds: Number(event.target.value),
              })}
              onBlur={(event) => applyReminderSettings({
                ...reminderSettingsDraft.current,
                autoDismissSeconds: Number(event.currentTarget.value),
              })}
            />
            <span>{t("secondsUnit")}</span>
          </label>
        </div>
        <div className="setting-row quiet-hours-row">
          <span className={`setting-icon ${reminderSettings.quietHours.enabled ? "quiet" : ""}`}>
            <Moon size={19} />
          </span>
          <div className="setting-copy">
            <strong>{t("quietHours")}</strong>
            <span>{t("quietHoursDescription", {
              start: reminderSettings.quietHours.startLocal,
              end: reminderSettings.quietHours.endLocal,
            })}</span>
          </div>
          <Toggle
            checked={reminderSettings.quietHours.enabled}
            disabled={reminderSettingsQuery.isLoading || saveReminderSettings.isPending}
            label={t("quietHours")}
            onChange={(enabled) => applyReminderSettings({
              ...reminderSettingsDraft.current,
              quietHours: { ...reminderSettingsDraft.current.quietHours, enabled },
            })}
          />
          {reminderSettings.quietHours.enabled && (
            <div className="quiet-time-fields">
              <label>
                <span>{t("quietStart")}</span>
                <input
                  type="time"
                  required
                  disabled={saveReminderSettings.isPending}
                  value={reminderSettings.quietHours.startLocal}
                  onChange={(event) => scheduleReminderSettingsSave({
                    ...reminderSettingsDraft.current,
                    quietHours: {
                      ...reminderSettingsDraft.current.quietHours,
                      startLocal: event.target.value,
                    },
                  })}
                  onBlur={(event) => applyReminderSettings({
                    ...reminderSettingsDraft.current,
                    quietHours: {
                      ...reminderSettingsDraft.current.quietHours,
                      startLocal: event.currentTarget.value,
                    },
                  })}
                />
              </label>
              <span aria-hidden="true">-</span>
              <label>
                <span>{t("quietEnd")}</span>
                <input
                  type="time"
                  required
                  disabled={saveReminderSettings.isPending}
                  value={reminderSettings.quietHours.endLocal}
                  onChange={(event) => scheduleReminderSettingsSave({
                    ...reminderSettingsDraft.current,
                    quietHours: {
                      ...reminderSettingsDraft.current.quietHours,
                      endLocal: event.target.value,
                    },
                  })}
                  onBlur={(event) => applyReminderSettings({
                    ...reminderSettingsDraft.current,
                    quietHours: {
                      ...reminderSettingsDraft.current.quietHours,
                      endLocal: event.currentTarget.value,
                    },
                  })}
                />
              </label>
            </div>
          )}
        </div>
      </div>

      <div className="settings-section notification-section">
        <h2>{t("systemNotifications")}</h2>
        <div className="setting-row">
          <span className="setting-icon"><Bell size={19} /></span>
          <div className="setting-copy">
            <strong>{t("systemNotifications")}</strong>
            <span>{t("systemNotificationsDescription")}</span>
            <span>
              {notificationState === "granted"
                ? t("allowed")
                : notificationState === "checking"
                  ? t("checking")
                  : notificationState === "unavailable"
                    ? t("systemUnavailable")
                    : t("notAllowed")}
            </span>
          </div>
          <div className="setting-actions">
            {notificationState === "denied" && (
              <button className="button secondary compact" type="button" onClick={() => void enableNotifications()}>
                {t("enableAction")}
              </button>
            )}
            <button
              className="button secondary compact"
              type="button"
              disabled={notificationState === "unavailable" || testingNotification}
              onClick={() => void testNotification()}
            >
              {testingNotification ? <LoaderCircle className="spin" size={15} /> : <Sparkles size={15} />}
              {t("test")}
            </button>
          </div>
        </div>
      </div>

      <div className="settings-section">
        <h2>{t("application")}</h2>
        <div className="setting-row display-name-row">
          <span className="setting-icon"><EyeOff size={19} /></span>
          <div className="setting-copy">
            <strong>{t("appDisplayName")}</strong>
            <span>{t("appDisplayNameDescription")}</span>
          </div>
          <label className="display-name-setting">
            <input
              type="text"
              minLength={1}
              maxLength={30}
              required
              spellCheck={false}
              disabled={reminderSettingsQuery.isLoading || saveReminderSettings.isPending}
              value={reminderSettings.appDisplayName}
              aria-label={t("appDisplayName")}
              onChange={(event) => scheduleReminderSettingsSave({
                ...reminderSettingsDraft.current,
                appDisplayName: event.target.value,
              })}
              onBlur={(event) => applyReminderSettings({
                ...reminderSettingsDraft.current,
                appDisplayName: event.currentTarget.value,
              })}
            />
          </label>
        </div>
        <div className="setting-row">
          <span className="setting-icon"><Power size={19} /></span>
          <div className="setting-copy">
            <strong>{t("autostart")}</strong>
            <span>{autostart.supported ? (autostart.error || t("autostartDescription")) : t("systemUnavailable")}</span>
          </div>
          {autostartQuery.isLoading ? <LoaderCircle className="spin" size={17} /> : (
            <Toggle
              checked={autostart.enabled}
              disabled={!autostart.supported || autostartQuery.isError || setAutostart.isPending}
              label={t("autostart")}
              onChange={(enabled) => setAutostart.mutate(enabled)}
            />
          )}
        </div>
        <div className="setting-row">
          <span className="setting-icon"><HardDrive size={19} /></span>
          <div className="setting-copy">
            <strong>{t("backgroundTray")}</strong>
            <span>{t("backgroundTrayDescription")}</span>
          </div>
          <span className="status-dot good" title={t("backgroundRunning")} />
        </div>
      </div>

      <div className="settings-section language-section">
        <h2>{t("language")}</h2>
        <div className="setting-row language-row">
          <span className="setting-icon"><Languages size={19} /></span>
          <div className="setting-copy">
            <strong>{t("language")}</strong>
            <span>{t("languageDescription")}</span>
          </div>
          <select
            className="language-select"
            value={locale}
            aria-label={t("language")}
            onChange={(event) => {
              setLocale(event.target.value as Locale);
              setMessage("");
            }}
          >
            {localeOptions.map((option) => <option key={option.code} value={option.code}>{option.label}</option>)}
          </select>
        </div>
      </div>

      <div className="settings-section update-section">
        <h2>{t("updates")}</h2>
        <div className="setting-row update-row">
          <span className="setting-icon"><RefreshCw size={19} /></span>
          <div className="setting-copy">
            <strong>{t("appUpdates")}</strong>
            <span>
              {updater.currentVersion
                ? t("currentVersion", { version: updater.currentVersion })
                : t("versionUnavailable")}
            </span>
            <span
              className={updater.error ? "update-error" : undefined}
              title={updater.error ?? undefined}
            >
              {updater.error ? t("updateFailed") : updateStatus}
            </span>
          </div>
          <div className="setting-actions">
            {updater.availableUpdate ? (
              <button
                className="button primary compact"
                type="button"
                disabled={!updater.supported || updateBusy}
                onClick={() => void updater.installUpdate()}
              >
                {updateBusy
                  ? <LoaderCircle className="spin" size={15} />
                  : <Download size={15} />}
                {updater.phase === "downloading"
                  ? t("downloadingUpdate")
                  : updater.phase === "installing"
                    ? t("installingUpdate")
                    : updater.phase === "restarting"
                      ? t("restartingUpdate")
                      : t("installUpdate")}
              </button>
            ) : (
              <button
                className="button secondary compact"
                type="button"
                disabled={!updater.supported || updateBusy}
                onClick={() => void updater.checkForUpdate()}
              >
                {updater.phase === "checking"
                  ? <LoaderCircle className="spin" size={15} />
                  : <RefreshCw size={15} />}
                {updater.phase === "checking" ? t("checkingForUpdates") : t("checkForUpdates")}
              </button>
            )}
          </div>
        </div>
        {updater.phase === "downloading" && (
          <div
            className={`update-progress ${updater.progressPercent === null ? "indeterminate" : ""}`}
            role="progressbar"
            aria-label={t("downloadingUpdate")}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={updater.progressPercent ?? undefined}
          >
            <span style={updater.progressPercent === null ? undefined : { width: `${updater.progressPercent}%` }} />
          </div>
        )}
        {updater.availableUpdate?.body && (
          <div className="update-notes">
            <strong>{t("releaseNotes")}</strong>
            <p>{updater.availableUpdate.body}</p>
          </div>
        )}
      </div>

      <div className="settings-section">
        <h2>{t("localData")}</h2>
        <div className="setting-row">
          <span className="setting-icon"><Database size={19} /></span>
          <div className="setting-copy">
            <strong>{t("reminderDatabase")}</strong>
            <span title={storageQuery.data?.databasePath}>
              {storageQuery.isLoading
                ? t("checking")
                : storageQuery.data?.healthy
                  ? t("dataHealthy", { count: storageQuery.data.reminderCount })
                  : t("dataNeedsCheck")}
            </span>
          </div>
          {storageQuery.data?.healthy
            ? <CheckCircle2 className="healthy-icon" size={19} aria-label={t("dataHealthy", { count: storageQuery.data?.reminderCount ?? 0 })} />
            : <span className="status-dot" title={t("unknownStatus")} />}
        </div>
      </div>

      <details className="settings-section appearance-section">
        <summary className="appearance-heading">
          <div>
            <h2>{t("appearance")}</h2>
            <p>{t("appearanceDescription")}</p>
          </div>
          <span className="appearance-summary-meta">
            <span className="theme-status"><Check size={13} />{t("themeSaved")}</span>
            <ChevronDown className="appearance-chevron" size={18} aria-hidden="true" />
          </span>
        </summary>
        <div className="theme-control-grid">
          <fieldset className="theme-control style-control">
            <legend>{t("style")}</legend>
            <div className="theme-option-grid">
              {styleOptions.map((option) => (
                <label className={`theme-option ${themeSettings.style === option.value ? "selected" : ""}`} key={option.value}>
                  <input
                    type="radio"
                    name="theme-style"
                    value={option.value}
                    checked={themeSettings.style === option.value}
                    onChange={() => updateTheme({ style: option.value })}
                  />
                  <span className={`style-preview style-${option.value}`} aria-hidden="true">
                    <span />
                    <i />
                    <b />
                  </span>
                  <strong>{t(option.labelKey)}</strong>
                </label>
              ))}
            </div>
          </fieldset>

          <fieldset className="theme-control">
            <legend>{t("accentColor")}</legend>
            <div className="accent-option-grid">
              {accentOptions.map((option) => (
                <label className={`accent-option ${themeSettings.accent === option.value ? "selected" : ""}`} key={option.value}>
                  <input
                    type="radio"
                    name="theme-accent"
                    value={option.value}
                    checked={themeSettings.accent === option.value}
                    onChange={() => updateTheme({ accent: option.value })}
                  />
                  <span className={`accent-swatch accent-${option.value}`} aria-hidden="true" />
                  <span>{t(option.labelKey)}</span>
                </label>
              ))}
            </div>
          </fieldset>

          <fieldset className="theme-control background-control">
            <legend>{t("backgroundStyle")}</legend>
            <div className="background-option-grid">
              {backgroundOptions.map((option) => (
                <label className={`background-option background-${option.value} ${themeSettings.background === option.value ? "selected" : ""}`} key={option.value}>
                  <input
                    type="radio"
                    name="theme-background"
                    value={option.value}
                    checked={themeSettings.background === option.value}
                    onChange={() => updateTheme({ background: option.value })}
                  />
                  <span aria-hidden="true" />
                  <strong>{t(option.labelKey)}</strong>
                </label>
              ))}
            </div>
          </fieldset>

          <div className="theme-preview" aria-hidden="true">
            <div className="theme-preview-topline"><span /><span /><span /></div>
            <div className="theme-preview-body">
              <span className="theme-preview-mark"><Coffee size={15} /></span>
              <div><b>{reminderSettings.appDisplayName}</b><small>{t("themePreview")}</small></div>
              <span className="theme-preview-accent" />
            </div>
          </div>
        </div>
      </details>
    </section>
  );
}

function OnboardingPage({ onComplete }: { onComplete: () => void }) {
  const { locale, t } = useI18n();
  const [error, setError] = useState("");
  const setup = useMutation({
    mutationFn: async (initialize: boolean) => {
      const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
      if (initialize) {
        return invoke<OnboardingStatusShape>("initialize_default_health_reminders", {
          input: { timezone, locale },
        });
      }
      return invoke<OnboardingStatusShape>("complete_onboarding");
    },
    onSuccess: () => {
      setError("");
      onComplete();
    },
    onError: (reason) => setError(readableError(reason, t)),
  });

  return (
    <section className="onboarding-page" aria-labelledby="onboarding-title">
      <div className="onboarding-mark" aria-hidden="true"><Coffee size={24} /></div>
      <span className="eyebrow">{t("welcome")}</span>
      <h1 id="onboarding-title">{t("onboardingTitle")}</h1>
      <p>{t("welcomeDescription")}</p>
      <p className="onboarding-detail">{t("onboardingDescription")}</p>
      <div className="onboarding-actions">
        <button
          className="button primary"
          type="button"
          disabled={setup.isPending}
          onClick={() => setup.mutate(true)}
        >
          {setup.isPending ? <LoaderCircle className="spin" size={17} /> : <Check size={17} />}
          {t("onboardingEnableReminders")}
        </button>
        <button
          className="button ghost"
          type="button"
          disabled={setup.isPending}
          onClick={() => setup.mutate(false)}
        >
          {t("skipOnboarding")}
        </button>
      </div>
      {error && <p className="form-error" role="alert">{error}</p>}
    </section>
  );
}

function MainApplication() {
  const { t } = useI18n();
  const updater = useAppUpdater();
  const queryClient = useQueryClient();
  const [activeTab, setActiveTab] = useState<Tab>("reminders");
  const onboardingQuery = useQuery({
    queryKey: ["onboarding-status"],
    queryFn: () => invoke<OnboardingStatusShape>("get_onboarding_status"),
    retry: false,
  });
  const reminderSettingsQuery = useQuery({
    queryKey: ["reminder-settings"],
    queryFn: () => invoke<ReminderSettingsShape>("get_reminder_settings", {
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    }),
    retry: false,
  });
  const appDisplayName = reminderSettingsQuery.data?.appDisplayName || DEFAULT_APP_DISPLAY_NAME;

  useEffect(() => {
    let disposed = false;
    const cleanups: Array<() => void> = [];
    const subscribe = async () => {
      const remindersChanged = await listen("reminders-changed", () => {
        void queryClient.invalidateQueries({ queryKey: ["reminders"] });
        void queryClient.invalidateQueries({ queryKey: ["storage-status"] });
      });
      if (disposed) remindersChanged();
      else cleanups.push(remindersChanged);
      const settingsChanged = await listen("settings-changed", () => {
        void queryClient.invalidateQueries({ queryKey: ["reminder-settings"] });
      });
      if (disposed) settingsChanged();
      else cleanups.push(settingsChanged);
    };
    void subscribe();
    return () => {
      disposed = true;
      cleanups.forEach((cleanup) => cleanup());
    };
  }, [queryClient]);

  if (onboardingQuery.data?.needsSetup && !onboardingQuery.data.hasReminders) {
    return (
      <main className="app-shell">
        <OnboardingPage onComplete={() => {
          void queryClient.invalidateQueries({ queryKey: ["onboarding-status"] });
          void queryClient.invalidateQueries({ queryKey: ["reminders"] });
          void queryClient.invalidateQueries({ queryKey: ["storage-status"] });
        }} />
      </main>
    );
  }

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand-lockup">
          <TeaLogo />
          <strong>{appDisplayName}</strong>
        </div>
        <nav className="main-tabs" aria-label={t("mainNavigation")}>
          <button
            type="button"
            className={activeTab === "reminders" ? "active" : ""}
            aria-current={activeTab === "reminders" ? "page" : undefined}
            onClick={() => setActiveTab("reminders")}
          >
            <Bell size={17} />{t("navReminders")}
          </button>
          <button
            type="button"
            className={activeTab === "settings" ? "active" : ""}
            aria-current={activeTab === "settings" ? "page" : undefined}
            onClick={() => setActiveTab("settings")}
          >
            <Settings size={17} />{t("navSettings")}
          </button>
        </nav>
        <span className="topbar-spacer" />
      </header>

      <div className="app-content">
        {updater.phase === "available" && updater.availableUpdate && (
          <div className="update-banner" role="status">
            <span className="update-banner-icon"><Download size={18} /></span>
            <div>
              <strong>{t("updateAvailable", { version: updater.availableUpdate.version })}</strong>
              <span>{t("updateReadyDescription")}</span>
            </div>
            <button className="button primary compact" type="button" onClick={() => setActiveTab("settings")}>
              <Download size={15} />{t("viewUpdate")}
            </button>
          </div>
        )}
        {activeTab === "reminders" ? <RemindersPage /> : <SettingsPage />}
      </div>
    </main>
  );
}

function App() {
  const isReminderSurface = useMemo(
    () => new URLSearchParams(window.location.search).get("surface") === "reminder",
    [],
  );
  return (
    <ThemeProvider>
      <I18nProvider>
        {isReminderSurface ? (
          <ReminderSurface />
        ) : (
          <UpdateProvider><MainApplication /></UpdateProvider>
        )}
      </I18nProvider>
    </ThemeProvider>
  );
}

export default App;
