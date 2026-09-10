import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type {
  ProviderUsageSnapshot,
  KimiAccountSnapshot,
  RefreshCompletePayload,
  RefreshStartedPayload,
} from "../types/bridge";
import {
  getCachedProviders,
  listKimiAccounts,
  refreshProviders,
  refreshProvidersIfStale,
} from "../lib/tauri";
import { providerSnapshotIdentity } from "../lib/providerGroups";

export interface UseProvidersOptions {
  /**
   * Delay the automatic stale-aware refresh on mount. Tray/menu surfaces use
   * this so opening the UI can paint and accept input before provider work
   * starts.
   */
  initialRefreshDelayMs?: number;
  /**
   * Whether mounting this hook should ask the backend for a stale-aware refresh.
   * Passive surfaces can turn this off when another timer already drives
   * freshness, while still receiving cached data and live provider events.
   */
  refreshOnMount?: boolean;
  /**
   * When true, the mount refresh bypasses stale-cache checks and refreshes all
   * enabled providers. Used by the tray/menu "refresh on open" setting.
   */
  forceRefreshOnMount?: boolean;
}

export interface UseProvidersResult {
  /** Current provider snapshots (updated live as each provider completes). */
  providers: ProviderUsageSnapshot[];
  kimiAccounts: KimiAccountSnapshot[];
  /** True while a refresh cycle is in progress. */
  isRefreshing: boolean;
  refreshingProviderIds: ReadonlySet<string>;
  /** Trigger a manual refresh. No-op if already refreshing. */
  refresh: () => void;
  /** Summary from the last completed refresh cycle, if any. */
  lastRefresh: RefreshCompletePayload | null;
  /** True when the hook has provider data that can stay visible during refresh. */
  hasCachedData: boolean;
  /** True after the initial cached-provider read has completed. */
  hasLoadedCache: boolean;
}

/**
 * Subscribe to live provider usage data.
 *
 * On mount the hook:
 *  1. Loads any cached providers already in AppState.
 *  2. Fires `refresh_providers` to kick off a fresh fetch cycle.
 *  3. Listens for `provider-updated` events and merges each snapshot
 *     into the local array (upsert by provider and credential identity).
 *  4. Listens for `refresh-started` / `refresh-complete` to track loading.
 */
export function useProviders(options: UseProvidersOptions = {}): UseProvidersResult {
  const [providers, setProviders] = useState<ProviderUsageSnapshot[]>([]);
  const [kimiAccounts, setKimiAccounts] = useState<KimiAccountSnapshot[]>([]);
  const [refreshingProviderIds, setRefreshingProviderIds] = useState<Set<string>>(
    new Set(),
  );
  const [lastRefresh, setLastRefresh] = useState<RefreshCompletePayload | null>(
    null,
  );
  const [hasLoadedCache, setHasLoadedCache] = useState(false);
  const refreshingRef = useRef(false);
  const pendingSnapshotsRef = useRef<Map<string, ProviderUsageSnapshot>>(new Map());
  const flushTimerRef = useRef<number | undefined>(undefined);
  const resetRefreshTimerRef = useRef<number | undefined>(undefined);
  const settingsReloadEpochRef = useRef(0);
  const kimiRevisionRef = useRef(0);
  const settingsReloadingRef = useRef(false);

  const mergeSnapshots = useCallback((snapshots: ProviderUsageSnapshot[]) => {
    if (snapshots.length === 0) return;
    setProviders((prev) => {
      const next = [...prev];
      const byId = new Map(
        next.map((provider, index) => [
          providerSnapshotIdentity(provider.providerId, provider.credentialId),
          index,
        ]),
      );
      for (const snapshot of snapshots) {
        const identity = providerSnapshotIdentity(
          snapshot.providerId,
          snapshot.credentialId,
        );
        const idx = byId.get(identity);
        if (idx !== undefined) {
          next[idx] = snapshot;
        } else {
          byId.set(identity, next.length);
          next.push(snapshot);
        }
      }
      return next;
    });
  }, []);

  const flushPendingSnapshots = useCallback(() => {
    if (flushTimerRef.current !== undefined) {
      window.clearTimeout(flushTimerRef.current);
      flushTimerRef.current = undefined;
    }
    const snapshots = Array.from(pendingSnapshotsRef.current.values());
    pendingSnapshotsRef.current.clear();
    mergeSnapshots(snapshots);
  }, [mergeSnapshots]);

  const queueSnapshot = useCallback((snapshot: ProviderUsageSnapshot) => {
    pendingSnapshotsRef.current.set(
      providerSnapshotIdentity(snapshot.providerId, snapshot.credentialId),
      snapshot,
    );
    if (settingsReloadingRef.current || flushTimerRef.current !== undefined) return;
    flushTimerRef.current = window.setTimeout(flushPendingSnapshots, 80);
  }, [flushPendingSnapshots]);

  const refresh = useCallback(() => {
    if (refreshingRef.current) return;
    refreshingRef.current = true;
    refreshProviders().catch(() => {
      refreshingRef.current = false;
      setRefreshingProviderIds(new Set());
    });
  }, []);

  useEffect(() => {
    let cancelled = false;

    // Load existing cache first.
    const initialEpoch = settingsReloadEpochRef.current;
    getCachedProviders()
      .then((cached) => {
        if (
          !cancelled &&
          initialEpoch === settingsReloadEpochRef.current &&
          cached.length > 0
        ) {
          mergeSnapshots(cached);
        }
      })
      .finally(() => {
        if (!cancelled) {
          setHasLoadedCache(true);
        }
      });
    const reloadKimiAccounts = () => {
      const revision = ++kimiRevisionRef.current;
      void Promise.resolve()
        .then(() => listKimiAccounts())
        .then((accounts) => {
          if (!cancelled && revision === kimiRevisionRef.current) {
            setKimiAccounts(accounts);
          }
        })
        .catch(() => {});
    };
    reloadKimiAccounts();

    // Event listeners.
    const unlistenUpdated = listen<ProviderUsageSnapshot>(
      "provider-updated",
      (event) => {
        if (!cancelled) {
          queueSnapshot(event.payload);
        }
      },
    );

    const unlistenKimiAccounts = listen<KimiAccountSnapshot[]>(
      "kimi-accounts-updated",
      (event) => {
        if (!cancelled) {
          kimiRevisionRef.current++;
          setKimiAccounts(event.payload);
        }
      },
    );

    const unlistenSettings = listen("settings-changed", () => {
      const epoch = ++settingsReloadEpochRef.current;
      settingsReloadingRef.current = true;
      if (flushTimerRef.current !== undefined) {
        window.clearTimeout(flushTimerRef.current);
        flushTimerRef.current = undefined;
      }
      pendingSnapshotsRef.current.clear();
      getCachedProviders()
        .then((cached) => {
          if (!cancelled && epoch === settingsReloadEpochRef.current) {
            // A settings mutation may remove or replace a credential identity.
            // Treat the backend cache as authoritative so deleted snapshots do
            // not survive indefinitely in the frontend merge map.
            setProviders(cached);
          }
        })
        .finally(() => {
          if (!cancelled && epoch === settingsReloadEpochRef.current) {
            settingsReloadingRef.current = false;
            flushPendingSnapshots();
          }
        });
      reloadKimiAccounts();
    });

    const unlistenStarted = listen<RefreshStartedPayload>("refresh-started", (event) => {
      if (!cancelled) {
        refreshingRef.current = true;
        setRefreshingProviderIds(new Set(event.payload.providerIds));
      }
    });

    const unlistenComplete = listen<RefreshCompletePayload>(
      "refresh-complete",
      (event) => {
        if (!cancelled) {
          if (!settingsReloadingRef.current) flushPendingSnapshots();
          refreshingRef.current = false;
          setRefreshingProviderIds(new Set());
          setLastRefresh(event.payload);
        }
      },
    );

    let initialRefreshTimer: number | undefined;

    const runInitialRefresh = () => {
      const refreshPromise = options.forceRefreshOnMount
        ? refreshProviders()
        : refreshProvidersIfStale();
      refreshPromise.catch(() => {
        if (!cancelled) {
          refreshingRef.current = false;
          setRefreshingProviderIds(new Set());
        }
      });
    };

    // Kick off the initial refresh, but let the backend reuse fresh cache.
    if (options.refreshOnMount !== false) {
      const delay = Math.max(0, options.initialRefreshDelayMs ?? 0);
      if (delay > 0) {
        initialRefreshTimer = window.setTimeout(runInitialRefresh, delay);
      } else {
        runInitialRefresh();
      }
    }

    return () => {
      cancelled = true;
      settingsReloadEpochRef.current += 1;
      settingsReloadingRef.current = false;
      if (initialRefreshTimer !== undefined) {
        window.clearTimeout(initialRefreshTimer);
      }
      if (flushTimerRef.current !== undefined) {
        window.clearTimeout(flushTimerRef.current);
        flushTimerRef.current = undefined;
      }
      if (resetRefreshTimerRef.current !== undefined) {
        window.clearTimeout(resetRefreshTimerRef.current);
        resetRefreshTimerRef.current = undefined;
      }
      pendingSnapshotsRef.current.clear();
      unlistenUpdated.then((fn) => fn());
      unlistenKimiAccounts.then((fn) => fn());
      unlistenSettings.then((fn) => fn());
      unlistenStarted.then((fn) => fn());
      unlistenComplete.then((fn) => fn());
    };
  }, [
    options.forceRefreshOnMount,
    options.initialRefreshDelayMs,
    options.refreshOnMount,
    flushPendingSnapshots,
    mergeSnapshots,
    queueSnapshot,
    refresh,
  ]);

  useEffect(() => {
    if (resetRefreshTimerRef.current !== undefined) {
      window.clearTimeout(resetRefreshTimerRef.current);
      resetRefreshTimerRef.current = undefined;
    }

    const now = Date.now();
    const nextReset = [
      ...providers.flatMap((provider) => [
        provider.primary.resetsAt,
        provider.secondary?.resetsAt,
        provider.modelSpecific?.resetsAt,
        provider.tertiary?.resetsAt,
        ...(provider.extraRateWindows ?? []).map((extra) => extra.window.resetsAt),
        provider.cost?.resetsAt,
      ]),
      ...kimiAccounts.map((account) => account.resetsAt),
    ]
      .filter((value): value is string => Boolean(value))
      .map((value) => Date.parse(value))
      .filter((time) => Number.isFinite(time) && time > now)
      .sort((a, b) => a - b)[0];

    if (nextReset === undefined) return;

    const delay = Math.max(5_000, nextReset - now + 1_000);
    resetRefreshTimerRef.current = window.setTimeout(() => {
      resetRefreshTimerRef.current = undefined;
      refresh();
    }, delay);

    return () => {
      if (resetRefreshTimerRef.current !== undefined) {
        window.clearTimeout(resetRefreshTimerRef.current);
        resetRefreshTimerRef.current = undefined;
      }
    };
  }, [providers, kimiAccounts, refresh]);

  return {
    providers,
    kimiAccounts,
    isRefreshing: refreshingProviderIds.size > 0,
    refreshingProviderIds,
    refresh,
    lastRefresh,
    hasCachedData: providers.length > 0,
    hasLoadedCache,
  };
}
