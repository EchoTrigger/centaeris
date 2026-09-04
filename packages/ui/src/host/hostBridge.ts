type HostEventHandler<TPayload> = (payload: TPayload) => void;

export type HostKind = "electron" | "web";

export type HostBridge = {
  kind: HostKind;
  invoke: <TResult>(
    command: string,
    payload?: Record<string, unknown>,
  ) => Promise<TResult>;
  listen: <TPayload>(
    eventName: string,
    handler: HostEventHandler<TPayload>,
  ) => Promise<() => void>;
};

declare global {
  interface Window {
    centaerisHost?: HostBridge;
  }
}

const hasElectronHostBridge = (): boolean => {
  return typeof window !== "undefined" && Boolean(window.centaerisHost);
};

export const isNativeHostRuntime = (): boolean => {
  return hasElectronHostBridge();
};

const webHostBridge: HostBridge = {
  kind: "web",
  invoke: async <TResult>(command: string): Promise<TResult> => {
    throw new Error(`${command} is unavailable without a native host`);
  },
  listen: async (): Promise<() => void> => {
    return () => undefined;
  },
};

export const getHostBridge = (): HostBridge => {
  if (hasElectronHostBridge() && window.centaerisHost) {
    return window.centaerisHost;
  }
  return webHostBridge;
};

export const invokeHost = async <TResult>(
  command: string,
  payload: Record<string, unknown> = {},
): Promise<TResult> => getHostBridge().invoke<TResult>(command, payload);

export const listenHost = async <TPayload>(
  eventName: string,
  handler: HostEventHandler<TPayload>,
): Promise<() => void> =>
  getHostBridge().listen<TPayload>(eventName, handler);
