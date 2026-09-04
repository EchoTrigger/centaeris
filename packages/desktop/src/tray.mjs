import { Menu, Tray } from "electron";

export const createTrayController = ({
  trayIconPath,
  showMainWindow,
  emitHostEvent,
  requestAppExit,
  reportActionFailure,
}) => {
  if (!trayIconPath) {
    throw new Error("trayIconPath is required");
  }
  if (typeof reportActionFailure !== "function") {
    throw new Error("tray reportActionFailure is required");
  }

  let tray = null;

  const runAction = (actionName, action) => {
    try {
      void Promise.resolve(action()).catch((error) => {
        reportActionFailure(actionName, error);
      });
    } catch (error) {
      reportActionFailure(actionName, error);
    }
  };

  const ensureTray = () => {
    if (tray) {
      return tray;
    }
    tray = new Tray(trayIconPath);
    tray.setToolTip("Centaeris");
    tray.setContextMenu(
      Menu.buildFromTemplate([
        {
          label: "New Chat",
          click: () => {
            runAction("new-chat", showMainWindow);
            emitHostEvent("centaeris/tray-new-chat", {});
          },
        },
        {
          label: "Open Centaeris",
          click: () => runAction("open", showMainWindow),
        },
        {
          label: "Exit",
          click: () => runAction("exit", requestAppExit),
        },
      ]),
    );
    tray.on("double-click", () => {
      runAction("double-click", showMainWindow);
    });
    return tray;
  };

  return {
    ensureTray,
    getTray: () => tray,
  };
};
