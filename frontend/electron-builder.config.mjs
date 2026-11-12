const hasNotarizationCredentials = Boolean(
  (
    process.env.APPLE_API_KEY
    && process.env.APPLE_API_KEY_ID
    && process.env.APPLE_API_ISSUER
  )
  || (
    process.env.APPLE_ID
    && process.env.APPLE_APP_SPECIFIC_PASSWORD
    && process.env.APPLE_TEAM_ID
  ),
);

const releaseBuild = process.env.YTU_RELEASE === '1';
const buildArch = process.env.YTU_ARCH || process.arch;

if (!['arm64', 'x64'].includes(buildArch)) {
  throw new Error(`Arquitectura macOS no soportada: ${buildArch}`);
}
if (buildArch !== process.arch) {
  throw new Error(
    `YTU_ARCH=${buildArch} no coincide con la arquitectura del proceso `
    + `(${process.arch}). El backend PyInstaller y FFmpeg son binarios nativos; `
    + 'ejecuta todo el build bajo la arquitectura de destino.',
  );
}

export default {
  appId: process.env.YTU_APP_ID || 'com.ytultimate.desktop',
  productName: 'YT Ultimate',
  artifactName: '${productName}-${version}-${arch}.${ext}',
  asar: true,
  files: [
    'dist/**/*',
    'electron/**/*',
    'package.json',
  ],
  directories: {
    output: 'release',
    buildResources: 'build',
  },
  extraResources: [
    {
      from: '.electron-backend',
      to: 'backend',
      filter: ['**/*'],
    },
    {
      from: '.electron-bin',
      to: 'bin',
      filter: ['**/*'],
    },
    {
      from: '../backend/models/audio-separator',
      to: 'models',
      filter: ['*.ckpt', '*.yaml', 'download_checks.json'],
    },
    {
      from: '../backend/PLUGIN_SDK.md',
      to: 'docs/PLUGIN_SDK.md',
    },
    {
      from: '../README.md',
      to: 'docs/README.md',
    },
    {
      from: '../backend/plugin-template',
      to: 'docs/plugin-template',
      filter: ['**/*', '!**/__pycache__/**', '!**/*.pyc'],
    },
    {
      from: '../backend/plugin-examples',
      to: 'docs/plugin-examples',
      filter: ['**/*', '!**/__pycache__/**', '!**/*.pyc'],
    },
  ],
  mac: {
    category: 'public.app-category.music',
    icon: 'build/icon.icns',
    target: [
      { target: 'dmg', arch: [buildArch] },
      { target: 'zip', arch: [buildArch] },
    ],
    darkModeSupport: true,
    // A local ad-hoc signature has no Team ID. Enabling library validation in
    // that mode prevents the frozen Python executable from loading its own
    // bundled framework. Formal releases opt in explicitly and are signed with
    // one Developer ID identity across the app and every nested Mach-O file.
    hardenedRuntime: releaseBuild,
    entitlements: 'build/entitlements.mac.plist',
    entitlementsInherit: 'build/entitlements.mac.inherit.plist',
    notarize: releaseBuild && hasNotarizationCredentials,
    forceCodeSigning: releaseBuild,
    gatekeeperAssess: false,
  },
  dmg: {
    title: 'Instalar YT Ultimate',
    icon: 'build/icon.icns',
    background: 'build/dmg-background.png',
    window: {
      width: 660,
      height: 430,
    },
    iconSize: 108,
    contents: [
      { x: 185, y: 225, type: 'file' },
      { x: 475, y: 225, type: 'link', path: '/Applications' },
    ],
  },
  fileAssociations: [
    {
      ext: ['mp3', 'm4a', 'wav', 'flac', 'aac', 'ogg', 'opus'],
      name: 'Archivo de audio',
      role: 'Viewer',
    },
    {
      ext: ['mp4', 'webm', 'mkv'],
      name: 'Archivo de vídeo',
      role: 'Viewer',
    },
    {
      ext: ['m3u', 'm3u8'],
      name: 'Playlist',
      role: 'Editor',
    },
  ],
};
