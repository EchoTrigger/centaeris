export async function startDesktop(initializeRuntime, createWindow) {
  // Host requests already share ensureRuntimeReady; rendering need not wait for it.
  await Promise.all([initializeRuntime(), createWindow()]);
}
