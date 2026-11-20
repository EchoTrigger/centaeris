export default {
  publish: null,
  win: { target: [{ target: "nsis", arch: ["x64"] }] },
  nsis: {
    oneClick: false,
    perMachine: false,
    allowToChangeInstallationDirectory: true,
    deleteAppDataOnUninstall: false,
    runAfterFinish: false,
  },
};
