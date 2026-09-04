const { contextBridge, ipcRenderer } = require("electron");

const invoke = (command, payload = {}) => {
  return ipcRenderer.invoke("host:invoke", { command, payload });
};

const listen = async (eventName, handler) => {
  if (typeof eventName !== "string" || !eventName.trim()) {
    throw new Error("host eventName is required");
  }
  if (typeof handler !== "function") {
    throw new Error("host event handler must be a function");
  }

  const normalizedEventName = eventName.trim();
  await ipcRenderer.invoke("host:subscribe", {
    eventName: normalizedEventName,
  });

  const listener = (_event, envelope) => {
    if (
      envelope &&
      envelope.eventName === normalizedEventName &&
      Object.prototype.hasOwnProperty.call(envelope, "payload")
    ) {
      handler(envelope.payload);
    }
  };

  ipcRenderer.on("host:event", listener);
  return () => {
    ipcRenderer.removeListener("host:event", listener);
  };
};

contextBridge.exposeInMainWorld("centaerisHost", {
  kind: "electron",
  invoke,
  listen,
});
