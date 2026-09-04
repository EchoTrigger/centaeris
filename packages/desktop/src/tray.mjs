import { Menu, Tray } from "electron";

export const createTrayController = ({
  trayIconPath,
  showMainWindow,
  emitHostEvent,
  requestAppExit,
}) => {
  if (!trayIconPath) {
    throw new Error("trayIconPath is required");
  }

  let tray = null;

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
            void showMainWindow();
            emitHostEvent("centaeris/tray-new-chat", {});
          },
        },
        {
          label: "Open Centaeris",
          click: () => void showMainWindow(),
        },
        {
          label: "Exit",
          click: () => void requestAppExit(),
        },
      ]),
    );
    tray.on("double-click", () => {
      void showMainWindow();
    });
    return tray;
  };

  return {
    ensureTray,
    getTray: () => tray,
  };
};
