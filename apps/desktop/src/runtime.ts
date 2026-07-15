import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type EventCallback } from "@tauri-apps/api/event";

export const isTauriRuntime = typeof window !== "undefined"
  && Boolean((window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);

const STORAGE_KEY = "takefive.browser.reminders";
const REMINDER_SETTINGS_KEY = "takefive.browser.reminderSettings";
const REMINDER_PREVIEW_KEY = "takefive.browser.reminderPreview";

type BrowserReminder = {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  revision: number;
  createdAtUtc: number;
  rule: Record<string, unknown>;
  nextTriggerAt: string | null;
};

function readReminders(): BrowserReminder[] {
  try {
    const value = JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? "[]");
    return Array.isArray(value) ? value : [];
  } catch {
    return [];
  }
}

function writeReminders(reminders: BrowserReminder[]) {
  window.localStorage.setItem(STORAGE_KEY, JSON.stringify(reminders));
}

function defaultReminderSettings() {
  return {
    autoDismissSeconds: 7,
    quietHours: {
      enabled: true,
      startLocal: "12:00",
      endLocal: "13:30",
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    },
  };
}

function readReminderSettings() {
  try {
    return JSON.parse(
      window.localStorage.getItem(REMINDER_SETTINGS_KEY) ?? JSON.stringify(defaultReminderSettings()),
    );
  } catch {
    return defaultReminderSettings();
  }
}

function nextTrigger(rule: Record<string, unknown>) {
  if (typeof rule.localDateTime === "string") {
    const timestamp = new Date(rule.localDateTime).getTime();
    return Number.isNaN(timestamp) ? null : new Date(timestamp).toISOString();
  }
  const interval = typeof rule.intervalMinutes === "number" ? rule.intervalMinutes : 60;
  return new Date(Date.now() + interval * 60_000).toISOString();
}

function browserRule(input: Record<string, unknown>, kind: string) {
  return {
    kind,
    timezone: String(input.timezone ?? Intl.DateTimeFormat().resolvedOptions().timeZone ?? "UTC"),
    weekdays: Array.isArray(input.weekdays) ? input.weekdays : [],
    times: kind === "fixed" ? [String(input.localTime ?? "10:30")] : [],
    localDateTime: input.localDateTime ?? null,
    intervalMinutes: typeof input.intervalMinutes === "number" ? input.intervalMinutes : null,
    activeWindowStart: input.activeWindowStart ?? null,
    activeWindowEnd: input.activeWindowEnd ?? null,
    excludedWindowStart: input.excludedWindowStart ?? null,
    excludedWindowEnd: input.excludedWindowEnd ?? null,
    anchorLocalDateTime: input.anchorLocalDateTime ?? null,
  };
}

function browserInvoke<T>(command: string, args?: Record<string, unknown>): T {
  const reminders = readReminders();
  switch (command) {
    case "list_reminders":
      return reminders as T;
    case "storage_status":
      return {
        schemaVersion: 1,
        healthy: true,
        reminderCount: reminders.length,
        databasePath: "Browser preview storage",
      } as T;
    case "get_onboarding_status":
      if (new URLSearchParams(window.location.search).get("onboarding") === "preview") {
        return { completed: false, needsSetup: true, hasReminders: false } as T;
      }
      return { completed: true, needsSetup: false, hasReminders: reminders.length > 0 } as T;
    case "get_autostart_status":
      return { available: false, enabled: false, error: null } as T;
    case "get_pause_status":
      return null as T;
    case "get_reminder_settings":
      return readReminderSettings() as T;
    case "update_reminder_settings": {
      const settings = args?.input ?? defaultReminderSettings();
      window.localStorage.setItem(REMINDER_SETTINGS_KEY, JSON.stringify(settings));
      return settings as T;
    }
    case "get_reminder_surface_payload":
      if (new URLSearchParams(window.location.search).get("surface") === "reminder") {
        const storedPreview = window.localStorage.getItem(REMINDER_PREVIEW_KEY);
        return (storedPreview ? JSON.parse(storedPreview) : {
          title: "Rest your eyes",
          body: "到时间啦，别忘了照顾一下自己。",
          occurrenceId: "browser-preview-occurrence",
          scheduledAt: new Date().toISOString(),
          preview: true,
        }) as T;
      }
      return null as T;
    case "preview_reminder": {
      const id = String(args?.id ?? "");
      const reminder = reminders.find((candidate) => candidate.id === id);
      if (!reminder) throw new Error(`reminder_not_found: ${id}`);
      const payload = {
        title: reminder.name,
        body: reminder.description.trim() || "到时间啦，别忘了照顾一下自己。",
        occurrenceId: `preview:${id}`,
        scheduledAt: new Date().toISOString(),
        preview: true,
      };
      window.localStorage.setItem(REMINDER_PREVIEW_KEY, JSON.stringify(payload));
      const url = new URL(window.location.href);
      url.search = "?surface=reminder";
      window.open(url, "takefive-reminder-preview", "popup,width=326,height=194");
      return payload as T;
    }
    case "dismiss_reminder_preview":
      window.localStorage.removeItem(REMINDER_PREVIEW_KEY);
      window.close();
      return undefined as T;
    case "set_autostart_enabled":
    case "pause_all":
    case "resume_all":
    case "complete_onboarding":
    case "initialize_default_health_reminders":
    case "complete_occurrence":
    case "skip_occurrence":
    case "snooze_occurrence":
    case "mark_occurrence_unhandled":
      return { completed: true, needsSetup: false, hasReminders: reminders.length > 0 } as T;
    case "set_reminder_enabled": {
      const id = String(args?.id ?? "");
      const next = reminders.map((reminder) => reminder.id === id
        ? { ...reminder, enabled: Boolean(args?.enabled), revision: reminder.revision + 1 }
        : reminder);
      writeReminders(next);
      return undefined as T;
    }
    case "update_reminder": {
      const input = (args?.input ?? {}) as Record<string, unknown>;
      const id = String(input.id ?? "");
      const current = reminders.find((reminder) => reminder.id === id);
      if (!current) throw new Error("reminder_not_found");
      if (current.revision !== Number(input.expectedRevision)) {
        throw new Error("reminder_revision_conflict");
      }
      const kind = String(input.kind ?? current.rule.kind ?? "fixed");
      const rule = browserRule(input, kind);
      const updated = {
        ...current,
        name: String(input.name ?? current.name),
        description: String(input.description ?? current.description),
        revision: current.revision + 1,
        rule,
        nextTriggerAt: nextTrigger(rule),
      };
      writeReminders(reminders.map((reminder) => reminder.id === id ? updated : reminder));
      return updated as T;
    }
    case "delete_reminder": {
      const id = String(args?.id ?? "");
      writeReminders(reminders.filter((reminder) => reminder.id !== id));
      return true as T;
    }
    case "create_reminder":
    case "create_one_shot_reminder":
    case "create_aligned_interval_reminder": {
      const input = (args?.input ?? {}) as Record<string, unknown>;
      const kind = command === "create_reminder" ? "fixed" : command === "create_one_shot_reminder" ? "oneShot" : "interval";
      const rule = browserRule(input, kind);
      const reminder: BrowserReminder = {
        id: typeof crypto.randomUUID === "function" ? crypto.randomUUID() : `${Date.now()}-${Math.random()}`,
        name: String(input.name ?? "Reminder"),
        description: String(input.description ?? ""),
        enabled: true,
        revision: 1,
        createdAtUtc: Date.now(),
        rule,
        nextTriggerAt: nextTrigger(rule),
      };
      writeReminders([...reminders, reminder]);
      return undefined as T;
    }
    default:
      return undefined as T;
  }
}

export function appInvoke<T>(command: string, args?: Record<string, unknown>) {
  return isTauriRuntime ? tauriInvoke<T>(command, args) : Promise.resolve(browserInvoke<T>(command, args));
}

export function appListen<T>(event: string, callback: EventCallback<T>) {
  return isTauriRuntime ? tauriListen<T>(event, callback) : Promise.resolve(() => undefined);
}
