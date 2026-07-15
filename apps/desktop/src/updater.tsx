import { getVersion } from "@tauri-apps/api/app";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { isTauriRuntime } from "./runtime";

const UPDATE_CHECK_STORAGE_KEY = "takefive.update.lastSuccessfulCheck";
const UPDATE_CHECK_INTERVAL_MS = 24 * 60 * 60 * 1_000;
const STARTUP_CHECK_DELAY_MS = 15_000;

export type UpdatePhase =
  | "idle"
  | "checking"
  | "upToDate"
  | "available"
  | "downloading"
  | "installing"
  | "restarting"
  | "error";

type UpdateContextValue = {
  supported: boolean;
  currentVersion: string | null;
  availableUpdate: Update | null;
  phase: UpdatePhase;
  progressPercent: number | null;
  error: string | null;
  checkForUpdate: () => Promise<void>;
  installUpdate: () => Promise<void>;
};

const UpdateContext = createContext<UpdateContextValue | null>(null);

function errorMessage(reason: unknown) {
  return reason instanceof Error ? reason.message : String(reason);
}

export function UpdateProvider({ children }: { children: ReactNode }) {
  const supported = isTauriRuntime && !import.meta.env.DEV;
  const [currentVersion, setCurrentVersion] = useState<string | null>(null);
  const [availableUpdate, setAvailableUpdate] = useState<Update | null>(null);
  const [phase, setPhase] = useState<UpdatePhase>("idle");
  const [progressPercent, setProgressPercent] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const updateRef = useRef<Update | null>(null);
  const operationInProgress = useRef(false);

  useEffect(() => {
    if (!isTauriRuntime) return;
    void getVersion().then(setCurrentVersion).catch(() => setCurrentVersion(null));
  }, []);

  const checkForUpdate = useCallback(async () => {
    if (!supported || operationInProgress.current) return;
    operationInProgress.current = true;
    setPhase("checking");
    setProgressPercent(null);
    setError(null);

    try {
      const nextUpdate = await check({ timeout: 20_000 });
      const previousUpdate = updateRef.current;
      if (previousUpdate && previousUpdate !== nextUpdate) {
        void previousUpdate.close().catch(() => undefined);
      }
      updateRef.current = nextUpdate;
      setAvailableUpdate(nextUpdate);
      setPhase(nextUpdate ? "available" : "upToDate");
      window.localStorage.setItem(UPDATE_CHECK_STORAGE_KEY, String(Date.now()));
    } catch (reason) {
      setPhase("error");
      setError(errorMessage(reason));
    } finally {
      operationInProgress.current = false;
    }
  }, [supported]);

  const installUpdate = useCallback(async () => {
    const update = updateRef.current;
    if (!supported || !update || operationInProgress.current) return;
    operationInProgress.current = true;
    setPhase("downloading");
    setProgressPercent(0);
    setError(null);

    let downloadedBytes = 0;
    let totalBytes: number | null = null;
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          downloadedBytes = 0;
          totalBytes = event.data.contentLength ?? null;
          setProgressPercent(totalBytes ? 0 : null);
          return;
        }
        if (event.event === "Progress") {
          downloadedBytes += event.data.chunkLength;
          if (totalBytes) {
            setProgressPercent(Math.min(100, Math.round((downloadedBytes / totalBytes) * 100)));
          }
          return;
        }
        setProgressPercent(100);
        setPhase("installing");
      });
      setPhase("restarting");
      await relaunch();
    } catch (reason) {
      setPhase("available");
      setError(errorMessage(reason));
    } finally {
      operationInProgress.current = false;
    }
  }, [supported]);

  useEffect(() => {
    if (!supported) return;
    const lastSuccessfulCheck = Number(window.localStorage.getItem(UPDATE_CHECK_STORAGE_KEY));
    if (Number.isFinite(lastSuccessfulCheck) && Date.now() - lastSuccessfulCheck < UPDATE_CHECK_INTERVAL_MS) {
      return;
    }
    const timeout = window.setTimeout(() => void checkForUpdate(), STARTUP_CHECK_DELAY_MS);
    return () => window.clearTimeout(timeout);
  }, [checkForUpdate, supported]);

  useEffect(() => () => {
    if (!operationInProgress.current) {
      void updateRef.current?.close().catch(() => undefined);
    }
  }, []);

  const value = useMemo<UpdateContextValue>(() => ({
    supported,
    currentVersion,
    availableUpdate,
    phase,
    progressPercent,
    error,
    checkForUpdate,
    installUpdate,
  }), [
    supported,
    currentVersion,
    availableUpdate,
    phase,
    progressPercent,
    error,
    checkForUpdate,
    installUpdate,
  ]);

  return <UpdateContext.Provider value={value}>{children}</UpdateContext.Provider>;
}

export function useAppUpdater() {
  const value = useContext(UpdateContext);
  if (!value) throw new Error("useAppUpdater must be used inside UpdateProvider");
  return value;
}
