/**
 * Copyright (c) 2026 Scott A Dixon
 *
 * A WebSocket a test plays the server side of.
 */
import { RpcSocket, type RpcSocketOptions, type WebSocketLike } from "@clerkenwell/client";

type Listener = (event: { readonly data: unknown }) => void;

export interface SentRequest {
  readonly id: string;
  readonly method: string;
  readonly params?: unknown;
}

export class FakeSocket implements WebSocketLike {
  readyState = 0;
  readonly sent: SentRequest[] = [];
  private readonly listeners = new Map<string, Array<{ listener: Listener; once: boolean }>>();

  addEventListener(type: "open" | "close" | "error", listener: () => void, options?: { once?: boolean }): void;
  addEventListener(type: "message", listener: (event: { readonly data: unknown }) => void): void;
  addEventListener(type: string, listener: Listener, options?: { once?: boolean }): void {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push({ listener, once: options?.once ?? false });
    this.listeners.set(type, listeners);
  }

  send(data: string): void {
    this.sent.push(JSON.parse(data) as SentRequest);
  }

  close(): void {
    this.drop();
  }

  /** The server accepts the connection. */
  accept(): void {
    this.readyState = 1;
    this.emit("open");
  }

  /**
   * The server cannot be reached. As from a real socket, pending promise
   * callbacks run between the error and the close.
   */
  refuse(): void {
    this.emit("error");
    setImmediate(() => this.drop());
  }

  /** The connection closes. */
  drop(): void {
    this.readyState = 3;
    this.emit("close");
  }

  /** The server sends a frame. */
  push(frame: object): void {
    this.emit("message", { data: JSON.stringify(frame) });
  }

  /** The server answers the request sent `index`th with `result`. */
  answer(index: number, result: unknown): void {
    this.push({ jsonrpc: "2.0", id: this.request(index).id, result });
  }

  /** The server refuses the request sent `index`th with `error`. */
  refuseRequest(index: number, error: { code: number; message: string; data?: unknown }): void {
    this.push({ jsonrpc: "2.0", id: this.request(index).id, error });
  }

  request(index: number): SentRequest {
    const request = this.sent[index];
    if (!request) throw new Error(`No request ${index} was sent.`);
    return request;
  }

  private emit(type: string, event: { readonly data: unknown } = { data: undefined }): void {
    const listeners = this.listeners.get(type) ?? [];
    this.listeners.set(type, listeners.filter(({ once }) => !once));
    for (const { listener } of listeners) listener(event);
  }
}

/** A socket whose every connection is a {@link FakeSocket}. */
export function fakeSocket(options: Partial<RpcSocketOptions> = {}) {
  const sockets: FakeSocket[] = [];
  const socket = new RpcSocket({
    url: "ws://server.test/projections",
    reconnectDelay: { initialMs: 5, maxMs: 20 },
    open: () => {
      const opened = new FakeSocket();
      sockets.push(opened);
      return opened;
    },
    ...options
  });
  const latest = (): FakeSocket => {
    const opened = sockets.at(-1);
    if (!opened) throw new Error("No connection was opened.");
    return opened;
  };
  return { socket, sockets, latest };
}

/** Lets pending promise callbacks run. */
export const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

export const pause = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
