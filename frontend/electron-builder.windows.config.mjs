import baseConfig from './electron-builder.config.mjs';

// The macOS sidecars staged for host builds never belong in this package.
const macOnlyResources = new Set(['.electron-backend', '.electron-bin']);

export const releasesRepository = Object.freeze({
  provider: 'github',
  owner: 'ElWapoteDev',
  repo: 'yt-ultimate-releases',
  releaseType: 'release',
});

export default {
  ...baseConfig,
  directories: {
    ...baseConfig.directories,
    // A separate resource folder keeps the DMG license out of the one-click
    // installer, which would otherwise show it and rewrite it with a BOM.
    buildResources: 'build/windows',
  },
  // Read by the main process to share the shortcut's AppUserModelID.
  extraMetadata: {
    ytuAppId: baseConfig.appId,
  },
  extraResources: [
    ...baseConfig.extraResources.filter((resource) => !macOnlyResources.has(resource.from)),
    {
      from: '../backend/build/windows/backend',
      to: 'backend',
      filter: ['**/*'],
    },
    {
      from: '.electron-bin-win',
      to: 'bin',
      filter: ['**/*'],
    },
  ],
  win: {
    target: [{ target: 'nsis', arch: ['x64'] }],
    icon: 'build/windows/icon.ico',
  },
  // Becomes resources/app-update.yml, which electron-updater reads to check
  // this public repository; it only holds installers, never the sources.
  // scripts/publish-windows-release.mjs uploads each version there.
  publish: [{ ...releasesRepository }],
  // Per-user one-click install: no administrator prompt, shortcuts on the
  // desktop and Start menu, and the app opens as soon as it finishes.
  nsis: {
    oneClick: true,
    perMachine: false,
    runAfterFinish: true,
    createDesktopShortcut: 'always',
    createStartMenuShortcut: true,
    shortcutName: baseConfig.productName,
    uninstallDisplayName: baseConfig.productName,
    deleteAppDataOnUninstall: false,
    // electron-updater only needs the elevation helper for per-machine installs.
    packElevateHelper: false,
    installerLanguages: ['es_ES', 'en_US'],
    language: '3082',
    // GitHub replaces spaces in asset names, so the file keeps the exact name
    // that latest.yml points the updater to.
    artifactName: 'YT-Ultimate-Setup-${version}.${ext}',
  },
};
