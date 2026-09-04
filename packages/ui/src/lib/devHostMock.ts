const DEV_WEB_MOCK_FLAG = "1";

export const isDevHostMockEnabled = (): boolean =>
  import.meta.env.DEV &&
  import.meta.env.VITE_CENTAERIS_ENABLE_WEB_MOCK_HOST === DEV_WEB_MOCK_FLAG;

export const requireDevHostMock = (capability: string): void => {
  if (isDevHostMockEnabled()) {
    return;
  }
  throw new Error(
    `${capability} requires native host. ` +
      "Set VITE_CENTAERIS_ENABLE_WEB_MOCK_HOST=1 only for explicit development mock testing.",
  );
};
