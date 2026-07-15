import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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
  Clock3,
  Coffee,
  Database,
  HardDrive,
  Languages,
  LoaderCircle,
  Pause,
  Play,
  Plus,
  Power,
  RotateCcw,
  Settings,
  Sparkles,
  Trash2,
  X,
} from "lucide-react";
import "./App.css";
import { I18nProvider, localeOptions, useI18n, type Locale } from "./i18n";

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

type Tab = "reminders" | "settings";
type ReminderRuleKind = "fixed" | "interval" | "oneShot";
type RepeatMode = "workdays" | "daily";
type NotificationState = "checking" | "granted" | "denied" | "unavailable";
interface ReminderDraft {
  name: string;
  timezone: string;
  kind: ReminderRuleKind;
  localTime: string;
  repeatMode: RepeatMode;
  oneShotLocalDateTime: string;
  intervalMinutes: string;
  anchorLocalDateTime: string;
  hasActiveWindow: boolean;
  activeWindowStart: string;
  activeWindowEnd: string;
  hasLunchBreak: boolean;
  lunchBreakStart: string;
  lunchBreakEnd: string;
}

const WORKDAYS = ["mon", "tue", "wed", "thu", "fri"];
const EVERY_DAY = [...WORKDAYS, "sat", "sun"];
const SURFACE_AUTO_DISMISS_MS = 8_000;

function localDateTimeAfter(minutes: number) {
  const date = new Date(Date.now() + minutes * 60_000);
  date.setSeconds(0, 0);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
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
    anchorLocalDateTime: localDateTimeAfter(0),
    hasActiveWindow: true,
    activeWindowStart: "09:00",
    activeWindowEnd: "18:00",
    hasLunchBreak: true,
    lunchBreakStart: "12:00",
    lunchBreakEnd: "13:30",
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
      <Coffee size={20} strokeWidth={2.35} />
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
    window = rule.excludedWindowStart && rule.excludedWindowEnd
      ? t("activeWindowLunchSummary", {
        start: rule.activeWindowStart,
        end: rule.activeWindowEnd,
        lunchStart: rule.excludedWindowStart,
        lunchEnd: rule.excludedWindowEnd,
      })
      : t("activeWindowSummary", { start: rule.activeWindowStart, end: rule.activeWindowEnd });
  }
  const anchor = formatRuleLocalDateTime(rule.anchorLocalDateTime, rule.timezone, locale, t("waitingCalculation"));
  return `${weekdays} · ${interval} · ${window} · ${t("anchorSummary", { time: anchor })}`;
}

function ReminderSurface() {
  const { locale, t } = useI18n();
  const [payload, setPayload] = useState<ReminderSurfacePayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [dismissing, setDismissing] = useState(false);
  const [error, setError] = useState("");
  const dismissingId = useRef<string | null>(null);

  const dismiss = useCallback(async (occurrenceId: string) => {
    if (dismissingId.current === occurrenceId) return;
    dismissingId.current = occurrenceId;
    setDismissing(true);
    setError("");

    await new Promise((resolve) => window.setTimeout(resolve, 140));
    try {
      await invoke("mark_occurrence_unhandled", { id: occurrenceId });
    } catch (reason) {
      const message = String(reason);
      if (!message.includes("occurrence_action_conflict") && !message.includes("occurrence_not_found")) {
        setError(t("dismissFailed"));
        setDismissing(false);
      }
      dismissingId.current = null;
    }
  }, [t]);

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
    const occurrenceId = payload.occurrenceId;
    const timer = window.setTimeout(() => {
      void dismiss(occurrenceId);
    }, SURFACE_AUTO_DISMISS_MS);
    const dismissOnKey = () => void dismiss(occurrenceId);
    window.addEventListener("keydown", dismissOnKey);

    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("keydown", dismissOnKey);
    };
  }, [dismiss, payload]);

  return (
    <main
      key={payload?.occurrenceId ?? "surface-loading"}
      className={`surface-shell ${dismissing ? "leaving" : ""}`}
      role="status"
      aria-live="assertive"
      aria-atomic="true"
      onPointerDown={() => payload && void dismiss(payload.occurrenceId)}
    >
      <span className="surface-mascot" aria-hidden="true">
        <Coffee size={23} strokeWidth={2.2} />
        <Sparkles className="surface-spark" size={12} />
      </span>

      {payload ? (
        <section className="surface-message" aria-labelledby="surface-title">
          <div className="surface-meta">
            <span>{t("teaReminder")}</span>
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
      {payload && <span className="surface-progress" aria-hidden="true" />}
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
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: () => Promise<void>;
}) {
  const { t } = useI18n();
  const [draft, setDraft] = useState(createDraft);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);

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
      if (!Number.isInteger(intervalMinutes) || intervalMinutes < 1 || intervalMinutes > 1440) {
        setError(t("intervalRange"));
        return;
      }
      if (zonedDateTimeToTimestamp(draft.anchorLocalDateTime, timezone) === null) {
        setError(t("invalidAnchor"));
        return;
      }
      if (
        draft.hasActiveWindow
        && (!draft.activeWindowStart || !draft.activeWindowEnd || draft.activeWindowStart === draft.activeWindowEnd)
      ) {
        setError(t("windowSame"));
        return;
      }
      if (
        draft.hasActiveWindow
        && draft.hasLunchBreak
        && !(
          draft.activeWindowStart < draft.lunchBreakStart
          && draft.lunchBreakStart < draft.lunchBreakEnd
          && draft.lunchBreakEnd < draft.activeWindowEnd
        )
      ) {
        setError(t("lunchOutsideWindow"));
        return;
      }
      command = "create_aligned_interval_reminder";
      input = {
        name,
        description: null,
        intervalMinutes,
        anchorLocalDateTime: draft.anchorLocalDateTime,
        timezone,
        weekdays: draft.repeatMode === "workdays" ? WORKDAYS : EVERY_DAY,
        activeWindowStart: draft.hasActiveWindow ? draft.activeWindowStart : null,
        activeWindowEnd: draft.hasActiveWindow ? draft.activeWindowEnd : null,
        excludedWindowStart: draft.hasActiveWindow && draft.hasLunchBreak ? draft.lunchBreakStart : null,
        excludedWindowEnd: draft.hasActiveWindow && draft.hasLunchBreak ? draft.lunchBreakEnd : null,
      };
    }

    setSaving(true);
    try {
      await invoke(command, { input });
      await onCreated();
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
          <span className="eyebrow">{t("newReminder")}</span>
          <h2 id="editor-title">{t("scheduleTime")}</h2>
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
            <div className="interval-fields full-span">
              <label className="field">
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
              <label className="field">
                <span>{t("alignedAnchor")}</span>
                <input
                  type="datetime-local"
                  required
                  value={draft.anchorLocalDateTime}
                  onChange={(event) => update("anchorLocalDateTime", event.target.value)}
                />
              </label>
            </div>
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
            <div className="window-toggle full-span">
              <span>{t("activeWindow")}</span>
              <Toggle
                checked={draft.hasActiveWindow}
                label={t("activeWindow")}
                onChange={(checked) => update("hasActiveWindow", checked)}
              />
            </div>
            {draft.hasActiveWindow && (
              <>
                <div className="time-window full-span">
                  <label className="field">
                    <span>{t("start")}</span>
                    <input type="time" value={draft.activeWindowStart} onChange={(event) => update("activeWindowStart", event.target.value)} />
                  </label>
                  <span aria-hidden="true">–</span>
                  <label className="field">
                    <span>{t("end")}</span>
                    <input type="time" value={draft.activeWindowEnd} onChange={(event) => update("activeWindowEnd", event.target.value)} />
                  </label>
                </div>
                <div className="window-toggle full-span">
                  <span>{t("lunchBreak")}</span>
                  <Toggle
                    checked={draft.hasLunchBreak}
                    label={t("lunchBreak")}
                    onChange={(checked) => update("hasLunchBreak", checked)}
                  />
                </div>
                {draft.hasLunchBreak && (
                  <div className="time-window full-span">
                    <label className="field">
                      <span>{t("lunchStart")}</span>
                      <input type="time" value={draft.lunchBreakStart} onChange={(event) => update("lunchBreakStart", event.target.value)} />
                    </label>
                    <span aria-hidden="true">–</span>
                    <label className="field">
                      <span>{t("lunchEnd")}</span>
                      <input type="time" value={draft.lunchBreakEnd} onChange={(event) => update("lunchBreakEnd", event.target.value)} />
                    </label>
                  </div>
                )}
              </>
            )}
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
            {t("saveReminder")}
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

  return (
    <div className={`reminders-layout ${editorOpen ? "with-editor" : ""}`}>
      <section className="page-pane" aria-labelledby="reminders-title">
        <div className="page-heading">
          <div>
            <span className="eyebrow">{t("reminderCount", { count: remindersQuery.data?.length ?? 0 })}</span>
            <h1 id="reminders-title">{t("navReminders")}</h1>
          </div>
          <button className="button primary" type="button" onClick={() => setEditorOpen(true)}>
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
            <button className="button secondary" type="button" onClick={() => setEditorOpen(true)}>
              <Plus size={17} />{t("newReminder")}
            </button>
          </div>
        )}
        {remindersQuery.isSuccess && remindersQuery.data.length > 0 && (
          <div className="reminder-list" aria-label={t("reminderList")}>
            {remindersQuery.data.map((reminder) => {
              const toggling = setEnabled.isPending && setEnabled.variables?.reminder.id === reminder.id;
              const deleting = deleteReminder.isPending && deleteReminder.variables === reminder.id;
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
                    {toggling
                      ? <LoaderCircle className="spin row-loader" size={17} />
                      : (
                        <Toggle
                          checked={reminder.enabled}
                          disabled={setEnabled.isPending || deleteReminder.isPending}
                          label={reminder.enabled ? t("disable", { name: reminder.name }) : t("enable", { name: reminder.name })}
                          onChange={(enabled) => setEnabled.mutate({ reminder, enabled })}
                        />
                      )}
                    <button
                      className="icon-button danger"
                      type="button"
                      title={t("delete", { name: reminder.name })}
                      disabled={deleteReminder.isPending || setEnabled.isPending}
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
          onClose={() => setEditorOpen(false)}
          onCreated={async () => {
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
  const queryClient = useQueryClient();
  const [notificationState, setNotificationState] = useState<NotificationState>("checking");
  const [testingNotification, setTestingNotification] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");

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

  const autostart = normalizeAutostart(autostartQuery.data);
  const pause = normalizePause(pauseQuery.data);

  useEffect(() => {
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

  const enableNotifications = async () => {
    setError("");
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
    try {
      let granted = await isPermissionGranted();
      if (!granted) {
        granted = (await requestPermission()) === "granted";
        setNotificationState(granted ? "granted" : "denied");
      }
      if (!granted) throw new Error(t("notificationPermissionOff"));
      sendNotification({ title: t("appName"), body: t("notificationReady") });
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

      <div className="settings-section">
        <h2>{t("notifications")}</h2>
        <div className="setting-row">
          <span className="setting-icon"><Bell size={19} /></span>
          <div className="setting-copy">
            <strong>{t("systemNotifications")}</strong>
            <span>{notificationState === "granted" ? t("allowed") : notificationState === "checking" ? t("checking") : t("notAllowed")}</span>
          </div>
          <div className="setting-actions">
            {notificationState !== "granted" && (
              <button className="button secondary compact" type="button" onClick={() => void enableNotifications()}>{t("enableAction")}</button>
            )}
            <button className="button secondary compact" type="button" disabled={testingNotification} onClick={() => void testNotification()}>
              {testingNotification ? <LoaderCircle className="spin" size={15} /> : <Sparkles size={15} />}
              {t("test")}
            </button>
          </div>
        </div>
        <div className="setting-row pause-row">
          <span className={`setting-icon ${pause.active ? "paused" : ""}`}><Pause size={19} /></span>
          <div className="setting-copy">
            <strong>{t("allReminders")}</strong>
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

      <div className="settings-section">
        <h2>{t("application")}</h2>
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
    </section>
  );
}

function MainApplication() {
  const { t } = useI18n();
  const queryClient = useQueryClient();
  const [activeTab, setActiveTab] = useState<Tab>("reminders");

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
    };
    void subscribe();
    return () => {
      disposed = true;
      cleanups.forEach((cleanup) => cleanup());
    };
  }, [queryClient]);

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand-lockup">
          <TeaLogo />
          <strong>{t("appName")}</strong>
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
    <I18nProvider>
      {isReminderSurface ? <ReminderSurface /> : <MainApplication />}
    </I18nProvider>
  );
}

export default App;
