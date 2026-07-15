import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type EventCallback } from "@tauri-apps/api/event";

export const isTauriRuntime = typeof window !== "undefined"
  && Boolean((window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);

const STORAGE_KEY = "takefive.browser.reminders";

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

function nextTrigger(rule: Record<string, unknown>) {
  if (typeof rule.localDateTime === "string") {
    const timestamp = new Date(rule.localDateTime).getTime();
    return Number.isNaN(timestamp) ? null : new Date(timestamp).toISOString();
  }
  const interval = typeof rule.intervalMinutes === "number" ? rule.intervalMinutes : 60;
  return new Date(Date.now() + interval * 60_000).toISOString();
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
      return { completed: true, needsSetup: false, hasReminders: reminders.length > 0 } as T;
    case "get_autostart_status":
      return { available: false, enabled: false, error: null } as T;
    case "get_pause_status":
      return null as T;
    case "get_reminder_surface_payload":
      return null as T;
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
      const rule: Record<string, unknown> = {
        kind,
        timezone: String(input.timezone ?? Intl.DateTimeFormat().resolvedOptions().timeZone ?? "UTC"),
        weekdays: Array.isArray(input.weekdays) ? input.weekdays : [],
        times: command === "create_reminder" ? [String(input.localTime ?? "10:30")] : [],
        localDateTime: input.localDateTime ?? null,
        intervalMinutes: typeof input.intervalMinutes === "number" ? input.intervalMinutes : null,
        activeWindowStart: input.activeWindowStart ?? null,
        activeWindowEnd: input.activeWindowEnd ?? null,
        excludedWindowStart: input.excludedWindowStart ?? null,
        excludedWindowEnd: input.excludedWindowEnd ?? null,
        anchorLocalDateTime: input.anchorLocalDateTime ?? null,
      };
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

