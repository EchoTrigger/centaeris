const PROCESS_TIMEOUT_MS = 30_000;

export const waitForExit = async (child) => {
  if (child.exitCode !== null || child.signalCode !== null) {
    return { code: child.exitCode, signal: child.signalCode };
  }
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      child.kill();
      reject(new Error(`Runtime Host did not exit within ${PROCESS_TIMEOUT_MS}ms`));
    }, PROCESS_TIMEOUT_MS);
    child.once("exit", (code, signal) => {
      clearTimeout(timeout);
      resolve({ code, signal });
    });
  });
};

export const assertCleanExit = async (child, label) => {
  if (!child) return;
  const { code, signal } = await waitForExit(child);
  if (code !== 0 || signal !== null) {
    throw new Error(`${label} did not exit cleanly: code=${code} signal=${signal}`);
  }
};

// Every cleanup runs, and secondary failures retain the original error object.
export const withCleanup = async (operation, ...cleanups) => {
  const errors = [];
  let result;
  try {
    result = await operation();
  } catch (error) {
    errors.push(error);
  }
  for (const cleanup of cleanups) {
    try {
      await cleanup();
    } catch (error) {
      errors.push(error);
    }
  }
  if (errors.length === 1) throw errors[0];
  if (errors.length > 1) {
    throw new AggregateError(errors, "Runtime smoke operation and cleanup failed", { cause: errors[0] });
  }
  return result;
};
