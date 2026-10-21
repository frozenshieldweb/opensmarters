// SPDX-License-Identifier: GPL-3.0-or-later
// Every string on the page (docs/LAUNCH_PROMPT.md §A2, §A6). Version-dependent values
// (version, sizes, hashes, URLs, which features ship) come from release.js and are put
// into the {placeholders} here.
//
// Voice (DESIGN.md, LAUNCH_PROMPT §0): calm, plain, warm, honest; short sentences; no
// exclamation marks, no hype. Claims must be backed by README.md or docs/SECURITY.md.

/** The product name: always lowercase, even at the start of a sentence. */
export const PRODUCT = "opensmarters";
const P = PRODUCT;

export const CONTENT = {
  lang: "en",
  meta: {
    title: `${P}: a secure, open-source TV player for your own subscription`,
    description:
      `${P} is a free, open-source TV player for Windows. Use it with your own Xtream Codes or ` +
      "M3U subscription: native and fast, made for the keyboard and the sofa, with no telemetry and no accounts.",
    ogTitle: `${P}: a secure, open-source TV player`,
    ogDescription: "A free, open-source player for your own Xtream Codes or M3U subscription. Native and fast, made for the sofa, and private by design.",
    ogImageAlt: `The ${P} wordmark and the line "Make room for good stories." on a warm, dark background.`,
    // The public address of this page (GitHub Pages, deployed by .github/workflows/pages.yml).
    // Open Graph wants absolute image URLs.
    siteUrl: "https://frozenshieldweb.github.io/opensmarters/",
    manifestName: P,
    manifestDescription: "A secure, open-source TV player for your own subscription.",
  },

  skip: "Skip to download",
  brand: P,
  symbolLabel: `${P} symbol`,

  headline: ["Make room", "for good stories."],
  subhead:
    "A free, open-source player for your own Xtream Codes or M3U subscription. " +
    "Native and fast, made for the sofa, and private by design.",

  download: {
    heading: "Download",
    primary: "Download for Windows",
    zipTag: "zip",
    zipTagLabel: "portable zip",
    // Under the primary button. {size} is in MB (1,000,000 bytes), like the release notes.
    caption: "v{version} · {size} MB · Windows 10/11 x64 · No admin rights needed",
    // Phones and tablets: the download is for a PC.
    mobile: "Get it on your Windows PC",
    // macOS, Linux, ChromeOS…
    other: "Windows only for now",
    buildFromSource: "Build from source",
    portable: "Portable zip",
    viewSource: "View source",
    sha: "SHA-256",
    newTab: "(opens in a new tab)",
    // The spec's line, made exact: passwords do go to the viewer's own provider.
    note: "Your passwords stay in Windows Credential Manager and go only to your provider.",
    unsigned: "Windows may warn that the publisher is unknown. Check the SHA-256 above.",
    toastDownloading: "Downloading v{version} · check it with SHA-256: {short}",
    toastCopyHash: "Copy SHA-256",
    linkCopied: "Link copied. Open it on your PC.",
    linkCopyFailed: "Open this page on your PC: {url}",
    todoLink: "This link isn't set yet (TODO in site/release.js).",
  },

  sha: {
    title: "SHA-256 checksums",
    intro: "Compare these with the file you downloaded.",
    installer: "Installer",
    portable: "Portable zip",
    copy: "Copy",
    copyLabel: "Copy the SHA-256 of {file}",
    copied: "Copied",
    sumsLink: "All checksums: {file}",
    howTo: "In PowerShell:",
    command: "Get-FileHash .\\{file}",
    requirements: "Needs {requirements}.",
    close: "Close",
  },

  // Trust chips (DESIGN §6.6), each with a one-line explanation.
  trustLabel: "Why you can trust it",
  trust: [
    { id: "open", icon: "CODE", label: "Open source · GPL-3.0", tip: "Free software: read, build and change the code under GPL-3.0-or-later." },
    { id: "telemetry", icon: "EYE_SLASH", label: "No telemetry, no accounts", tip: "No analytics and no sign-up. The app talks to your provider, and to TMDB only if you turn it on." },
    { id: "native", icon: "CPU", label: "Native app, no web engine", tip: "Built in Rust. Provider text is drawn as text and never run as code." },
    { id: "passwords", icon: "KEY", label: "Passwords in Credential Manager", tip: "Passwords and playlist links live in Windows Credential Manager, never in files or logs." },
    { id: "smooth", icon: "GAUGE", label: "Smooth with 35,000 titles", tip: "Films and series load per category, so a big catalogue never blocks the screen." },
    { id: "keyboard", icon: "KEYBOARD", label: "Keyboard, remote and mouse", tip: "Arrows and Enter everywhere, a TV mode for the sofa, and full mouse support." },
  ],

  // Feature beats; which ones show depends on release.js (since <= version).
  featuresLabel: "What it does",
  featureHint: "Show it in the preview",
  features: {
    live: { icon: "TELEVISION_SIMPLE", label: "Live TV and guide" },
    vod: { icon: "FILM_STRIP", label: "Films and series with resume" },
    catchup: { icon: "CLOCK_COUNTER_CLOCKWISE", label: "Catch-up" },
    parental: { icon: "LOCK_SIMPLE", label: "Parental control" },
    rewind: { icon: "REWIND", label: "Pause and rewind live TV" },
    record: { icon: "RECORD", label: "Record and download" },
    multi: { icon: "SQUARES_FOUR", label: "Multiscreen" },
    sleep: { icon: "MOON", label: "Sleep timer and reminders" },
  },

  footer: {
    line:
      `${P} ships with no channels, content or provider. Use it with a subscription you're entitled to. ` +
      "Independent open-source project, not affiliated with IPTV Smarters or WHMCS Smarters.",
    licence: "Licence",
    security: "Security",
    source: "Source",
  },

  notices: {
    todo: "Links not set yet: TODO in site/release.js",
    preview: "Preview of v{version}, not released",
  },

  toastClose: "Dismiss",

  // ---- The interactive replica ------------------------------------------------------
  replica: {
    label: `Interactive preview of ${P}`,
    tabsLabel: "Preview scenes",
    tabs: { live: "Live TV", guide: "Guide", player: "Player" },
    hint: "← → ↑ ↓ move · Enter play · PgUp/PgDn change channel",
    hintTouch: "Tap to choose · tap again to play",
    hintEsc: "Esc leaves the preview",
    fallbackAlt: `${P} Live TV screen: a channel list with programme progress, and a preview of the playing channel.`,

    allChannels: "All channels",
    channelsCount: "{n} channels",
    onNow: "On now",
    playingInPreview: "Playing in preview",
    now: "Now",
    next: "Next",
    minRemaining: "{n} min remaining",
    fullscreen: "Fullscreen",
    guide: "Guide",
    pressEnter: "Press Enter to play",
    tapToPlay: "Tap again to play",
    live: "Live",
    goLive: "Go live",
    today: "Today",
    firstChannel: "First channel",
    lastChannel: "Last channel",
    noChannel: "No channel {n} in this list",
    startsAt: "Starts at {time}",
    catchupHere: "Catch-up: Enter plays it from the start",
    onNowLabel: "on now",
    minutes: "{n} min",
    sleepIn: "Sleep in 15 min",
    recording: "Recording",
    multiscreen: "Multiscreen · sound follows focus",
    paused: "Paused",
    back: "Back",
    playPause: "Play or pause",
    back10: "Back 10 seconds",
    fwd10: "Forward 10 seconds",
    tracks: "Audio and subtitles",
    aspect: "Aspect",
    volume: "Volume",
    ten: "10",
    // Announcements for screen readers (the visual state says the same).
    sayRow: "{num} {name}, {title}",
    sayPlaying: "Playing {name}",
    sayScene: "{scene} scene",

    // Parental control (DESIGN §6.14) on the demo provider's locked category.
    lockedCategory: "Adult Night",
    lockedTitle: "This category is locked",
    lockedBody: "Enter your PIN to unlock it for this session.",
    pinClear: "Clear",
    pinDelete: "Delete",

    // Films and series: the resume state.
    vodTitle: "The Quiet Lighthouse",
    vodMeta: "2026 · Drama",
    vodElapsed: "42:10",
    vodRemaining: "−1:02:13",

    // The app's synthetic demo data (crates/fake-panel): no real channels or shows.
    channels: [
      "Glacier Live",
      "Silverbay Politics",
      "Larkspur Headlines",
      "Oakhaven Politics",
      "Harborlight News HD",
      "Ravenmoor Politics",
      "Zephyr Headlines",
      "Copperfield Live",
    ],
    programmes: [
      "Morning Briefing",
      "World Desk",
      "Business Today",
      "Evening Bulletin",
      "Weather Watch",
      "Late Edition",
      "Headlines Now",
      "The Debate Room",
    ],
    repeat: "{title} — Repeat",
    synopsis: "{title}: a synthetic programme on {channel}, invented for this preview like everything in it.",
  },
};
