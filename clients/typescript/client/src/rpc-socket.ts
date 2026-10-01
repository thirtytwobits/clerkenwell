/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * JSON-RPC 2.0 over one WebSocket: each request settled by its response,
 * notifications delivered to listeners, and the socket reopened with backoff
 * after it closes.
 */
import type { ProjectionErrorEnvelope } from "./protocol.js";

/** The WebSocket a socket runs over: what browsers and Node both provide. */
export interface WebSocketLike {
  readonly readyState: number;
  send(data: string): void;
  close(): void;
  addEventListener(type: "open" | "close" | "error", listener: () => void, options?: { once?: boolean }): void;
  addEventListener(type: "message", listener: (event: { readonly data: unknown }) => void): void;
}

/** `WebSocket.readyState` values. */
const OPEN = 1;
const CLOSED = 3;

/** An error a server answered a request with. */
export class RpcError extends Error {
  readonly code: number;
  readonly data: unknown;
  /** How the projection protocol describes the refusal, when it answered a projection command. */
  readonly projectionError: ProjectionErrorEnvelope | null;

  constructor(error: { code: number; message: string; data?: unknown }) {
    super(error.message);
    this.name = "RpcError";
    this.code = error.code;
    this.data = error.data;
    this.projectionError = projectionErrorOf(error.data);
  }
}

function projectionErrorOf(data: unknown): ProjectionErrorEnvelope | null {
  if (!isRecord(data) || !isRecord(data.projection_error)) return null;
  const envelope = data.projection_error;
  return typeof envelope.code === "string"
    && typeof envelope.message === "string"
    && typeof envelope.operation === "string"
    && typeof envelope.retryable === "boolean"
    ? envelope as unknown as ProjectionErrorEnvelope
    : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** A message the server pushes. */
export interface RpcNotification {
  readonly method: string;
  readonly params?: unknown;
}

export type RpcSocketStatus = "disconnected" | "connecting" | "connected" | "retry_wait";

export interface RpcSocketState {
  readonly status: RpcSocketStatus;
  /** When the next attempt to reopen is made, in epoch milliseconds, while waiting to retry. */
  readonly retryAt: number | null;
}

export interface RpcSocketOptions {
  readonly url: string;
  /** Whether the socket reopens after it closes unexpectedly. Defaults to `true`. */
  readonly reconnectOnClose?: boolean;
  /** The delay before the first attempt to reopen, doubled after each failure up to `maxMs`. */
  readonly reconnectDelay?: { readonly initialMs: number; readonly maxMs: number };
  /** Opens the WebSocket; the platform's `WebSocket` when absent. */
  readonly open?: (url: string) => WebSocketLike;
  /** The message a request pending when the socket closes is rejected with. */
  readonly closedMessage?: string;
  /** The message a failed attempt to open the socket is rejected with. */
  readonly unreachableMessage?: string;
  /** Builds the error a refused request is rejected with; an {@link RpcError} when absent. */
  readonly refusal?: (error: { code: number; message: string; data?: unknown }) => RpcError;
  /** Called each time the socket closes. */
  readonly onClose?: () => void;
  /** Called each time the socket opens after having closed. */
  readonly onReopen?: () => void;
}

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
}

interface Response {
  readonly id?: unknown;
  readonly result?: unknown;
  readonly error?: { code: number; message: string; data?: unknown };
}

/** One JSON-RPC connection to a server. */
export class RpcSocket {
  private readonly options: RpcSocketOptions;
  private readonly delay: { readonly initialMs: number; readonly maxMs: number };
  private socket: WebSocketLike | null = null;
  private opening: Promise<void> | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelayMs: number;
  private hasOpened = false;
  private allowReconnect: boolean;
  private state: RpcSocketState = { status: "disconnected", retryAt: null };
  private nextRequestId = 1;
  private readonly pending = new Map<string, PendingRequest>();
  private readonly notificationListeners = new Set<(notification: RpcNotification) => void>();
  private readonly connectionListeners = new Set<(connected: boolean) => void>();
  private readonly stateListeners = new Set<(state: RpcSocketState) => void>();

  constructor(options: RpcSocketOptions) {
    this.options = options;
    this.delay = options.reconnectDelay ?? { initialMs: 500, maxMs: 4_000 };
    this.reconnectDelayMs = this.delay.initialMs;
    this.allowReconnect = options.reconnectOnClose ?? true;
  }

  get url(): string {
    return this.options.url;
  }

  getConnectionState(): RpcSocketState {
    return this.state;
  }

  addNotificationListener(listener: (notification: RpcNotification) => void): () => void {
    this.notificationListeners.add(listener);
    return () => { this.notificationListeners.delete(listener); };
  }

  /** Registers a listener told `true` when the socket opens and `false` when it closes. */
  addConnectionListener(listener: (connected: boolean) => void): () => void {
    this.connectionListeners.add(listener);
    return () => { this.connectionListeners.delete(listener); };
  }

  /** Registers a listener for each change of state, first told the current one. */
  addConnectionStateListener(listener: (state: RpcSocketState) => void): () => void {
    this.stateListeners.add(listener);
    listener(this.state);
    return () => { this.stateListeners.delete(listener); };
  }

  /** Opens the socket, or waits for the attempt already under way. */
  connect(): Promise<void> {
    if (this.socket?.readyState === OPEN) return Promise.resolve();
    if (this.opening) return this.opening;
    this.allowReconnect = this.options.reconnectOnClose ?? true;
    this.setState({ status: "connecting", retryAt: null });
    this.opening = new Promise<void>((resolve, reject) => {
      const open: (url: string) => WebSocketLike = this.options.open ?? ((url) => new WebSocket(url));
      const socket = open(this.options.url);
      this.socket = socket;
      let opened = false;
      let settled = false;
      const unreachable = () => new Error(this.options.unreachableMessage ?? "Could not connect to the server.");
      socket.addEventListener("open", () => {
        const reopened = this.hasOpened;
        opened = true;
        settled = true;
        this.hasOpened = true;
        this.reconnectDelayMs = this.delay.initialMs;
        this.opening = null;
        this.connectionListeners.forEach((listener) => listener(true));
        this.setState({ status: "connected", retryAt: null });
        if (reopened) this.options.onReopen?.();
        resolve();
      });
      socket.addEventListener("message", (event) => {
        this.receive(String(event.data));
      });
      socket.addEventListener("error", () => {
        if (!settled && this.opening) {
          settled = true;
          this.opening = null;
          reject(unreachable());
        }
      });
      socket.addEventListener("close", () => {
        this.socket = null;
        this.connectionListeners.forEach((listener) => listener(false));
        this.options.onClose?.();
        if (!opened && !settled) {
          settled = true;
          this.opening = null;
          reject(unreachable());
        }
        this.rejectPending(new Error(this.options.closedMessage ?? "The server connection closed."));
        if (!this.allowReconnect) {
          this.setState({ status: "disconnected", retryAt: null });
          return;
        }
        this.scheduleReconnect();
      });
    });
    return this.opening;
  }

  /** Sends a request once the socket is open, resolving with its result. */
  async call<TResult = unknown>(method: string, params?: unknown): Promise<TResult> {
    await this.connect();
    return this.request<TResult>(method, params);
  }

  /** Closes the socket and stops reopening it. */
  async disconnect(): Promise<void> {
    this.allowReconnect = false;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.setState({ status: "disconnected", retryAt: null });
    const socket = this.socket;
    if (!socket) return;
    if (socket.readyState === CLOSED) {
      this.socket = null;
      return;
    }
    await new Promise<void>((resolve) => {
      socket.addEventListener("close", () => resolve(), { once: true });
      socket.addEventListener("error", () => resolve(), { once: true });
      try {
        socket.close();
      } catch {
        resolve();
      }
    });
  }

  private request<TResult>(method: string, params: unknown): Promise<TResult> {
    const id = String(this.nextRequestId++);
    const settled = new Promise<TResult>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
    });
    try {
      const socket = this.socket;
      if (!socket || socket.readyState !== OPEN) {
        throw new Error(this.options.closedMessage ?? "The server connection closed.");
      }
      socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, ...(params === undefined ? {} : { params }) }));
    } catch (error) {
      const pending = this.pending.get(id);
      this.pending.delete(id);
      pending?.reject(error);
    }
    return settled;
  }

  private receive(raw: string): void {
    const frame = JSON.parse(raw) as Response & Partial<RpcNotification>;
    if (typeof frame.method === "string") {
      const notification = frame as RpcNotification;
      this.notificationListeners.forEach((listener) => listener(notification));
      return;
    }
    const id = frame.id === undefined || frame.id === null ? "" : String(frame.id);
    const pending = this.pending.get(id);
    if (!pending) return;
    this.pending.delete(id);
    if (frame.error) {
      pending.reject(this.options.refusal?.(frame.error) ?? new RpcError(frame.error));
    } else {
      pending.resolve(frame.result);
    }
  }

  private rejectPending(error: Error): void {
    for (const pending of this.pending.values()) pending.reject(error);
    this.pending.clear();
  }

  private setState(next: RpcSocketState): void {
    if (this.state.status === next.status && this.state.retryAt === next.retryAt) return;
    this.state = next;
    this.stateListeners.forEach((listener) => listener(next));
  }

  private scheduleReconnect(): void {
    if (!this.allowReconnect || this.reconnectTimer !== null) return;
    this.setState({ status: "retry_wait", retryAt: Date.now() + this.reconnectDelayMs });
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      void this.connect().catch(() => {
        this.reconnectDelayMs = Math.min(this.reconnectDelayMs * 2, this.delay.maxMs);
        this.scheduleReconnect();
      });
    }, this.reconnectDelayMs);
  }
}
