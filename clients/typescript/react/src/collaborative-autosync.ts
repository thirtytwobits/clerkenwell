/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * React policy for automatically syncing collaborative authoring drafts.
 */
import React from "react";

export const DEFAULT_COLLABORATIVE_AUTOSYNC_DELAY_MS = 650;

export interface CollaborativeAutosyncOptions<TDocument> {
  /**
   * Stable key for the draft content that would be synced.
   *
   * When a non-retryable rejection occurs, the hook suppresses another attempt
   * for the same key until the author changes the draft.
   */
  draftKey: (draft: TDocument) => string;
  enabled?: boolean;
  isBlocked?: boolean;
  isDirty: boolean;
  isSyncing: boolean;
  delayMs?: number;
  draft: TDocument | null;
  syncDraft: (draft: TDocument) => Promise<boolean | void> | boolean | void;
}

export interface CollaborativeAutosyncHandle {
  clearRejectedDraft: () => void;
}

export function shouldScheduleCollaborativeAutosync(input: {
  enabled: boolean;
  hasDraft: boolean;
  isBlocked: boolean;
  isDirty: boolean;
  isSyncing: boolean;
  draftKey: string | null;
  lastRejectedDraftKey: string | null;
}): boolean {
  return input.enabled
    && input.hasDraft
    && input.isDirty
    && !input.isSyncing
    && !input.isBlocked
    && input.draftKey !== null
    && input.lastRejectedDraftKey !== input.draftKey;
}

export function rejectedCollaborativeAutosyncKeyAfterDraftChange(input: {
  currentDraftKey: string | null;
  lastRejectedDraftKey: string | null;
  lastSeenDraftKey: string | null;
}): string | null {
  return input.lastRejectedDraftKey !== null
    && input.lastSeenDraftKey !== null
    && input.currentDraftKey !== input.lastSeenDraftKey
    ? null
    : input.lastRejectedDraftKey;
}

export function useCollaborativeAutosync<TDocument>({
  delayMs = DEFAULT_COLLABORATIVE_AUTOSYNC_DELAY_MS,
  draft,
  draftKey,
  enabled = true,
  isBlocked = false,
  isDirty,
  isSyncing,
  syncDraft
}: CollaborativeAutosyncOptions<TDocument>): CollaborativeAutosyncHandle {
  const lastRejectedDraftKeyRef = React.useRef<string | null>(null);
  const lastSeenDraftKeyRef = React.useRef<string | null>(null);

  const clearRejectedDraft = React.useCallback((): void => {
    lastRejectedDraftKeyRef.current = null;
  }, []);

  React.useEffect(() => {
    const currentDraftKey = draft ? draftKey(draft) : null;
    lastRejectedDraftKeyRef.current = rejectedCollaborativeAutosyncKeyAfterDraftChange({
      currentDraftKey,
      lastRejectedDraftKey: lastRejectedDraftKeyRef.current,
      lastSeenDraftKey: lastSeenDraftKeyRef.current
    });
    lastSeenDraftKeyRef.current = currentDraftKey;

    if (!shouldScheduleCollaborativeAutosync({
      draftKey: currentDraftKey,
      enabled,
      hasDraft: draft !== null,
      isBlocked,
      isDirty,
      isSyncing,
      lastRejectedDraftKey: lastRejectedDraftKeyRef.current
    })) {
      return;
    }

    if (draft === null || currentDraftKey === null) {
      return;
    }

    const syncKey = currentDraftKey;
    if (lastRejectedDraftKeyRef.current === syncKey) {
      return;
    }

    const draftSnapshot = structuredClone(draft);
    const timer = window.setTimeout(() => {
      void Promise.resolve(syncDraft(draftSnapshot)).then((synced) => {
        if (synced === false) {
          lastRejectedDraftKeyRef.current = syncKey;
        } else {
          lastRejectedDraftKeyRef.current = null;
        }
      });
    }, delayMs);

    return () => {
      window.clearTimeout(timer);
    };
  }, [
    delayMs,
    draft,
    draftKey,
    enabled,
    isBlocked,
    isDirty,
    isSyncing,
    syncDraft
  ]);

  return { clearRejectedDraft };
}
