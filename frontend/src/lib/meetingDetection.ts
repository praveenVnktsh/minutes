export interface DetectedMeetingApp {
  name: string;
  detectedProcess: string;
  /** True when the app was not in the known list (generic fallback). */
  generic?: boolean;
}

const MEETING_APP_PATTERNS: Array<{ name: string; patterns: RegExp[] }> = [
  { name: 'Google Meet', patterns: [/^google meet$/i] },
  { name: 'Zoom', patterns: [/\bzoom(?:\.us| workplace)?\b/i] },
  { name: 'Microsoft Teams', patterns: [/\bmicrosoft teams\b/i, /^teams$/i] },
  { name: 'Cisco Webex', patterns: [/\bwebex\b/i] },
  { name: 'Slack Huddle', patterns: [/^slack$/i] },
  { name: 'Discord', patterns: [/^discord$/i] },
  { name: 'FaceTime', patterns: [/^facetime$/i] },
  { name: 'Amazon Chime', patterns: [/\bamazon chime\b/i] },
  { name: 'GoTo Meeting', patterns: [/\bgotomeeting\b/i, /\bgo to meeting\b/i] },
  { name: 'BlueJeans', patterns: [/\bbluejeans\b/i] },
  { name: 'WhatsApp', patterns: [/\bwhatsapp\b/i] },
  { name: 'Telegram', patterns: [/^telegram$/i] },
  { name: 'Signal', patterns: [/^signal$/i] },
];

/**
 * Apps that play audio but are not meetings. The generic fallback ignores
 * these so music/video playback (and browser audio, which is probed for Meet
 * separately) does not trigger a false prompt.
 */
const NON_MEETING_AUDIO_PATTERNS: RegExp[] = [
  /spotify/i,
  /^music$/i,
  /podcasts?/i,
  /^vlc$/i,
  /quicktime/i,
  /imovie/i,
  /garageband/i,
  /audacity/i,
  /^safari$/i,
  /^google chrome$/i,
  /^chrome$/i,
  /^microsoft edge$/i,
  /^edge$/i,
  /firefox/i,
  /brave/i,
  /\barc$/i,
  /chromium/i,
  /opera/i,
  /^minutes$/i,
  /^meetily$/i,
  /coreaudio/i,
  // Dictation and voice tools that open the mic for a few seconds at a time.
  /superwhisper/i,
  /wispr flow/i,
  /macwhisper/i,
  /voice control/i,
  /dictation/i,
  /voice memos/i,
  /^siri$/i,
  /^claude$/i,
  /^chatgpt$/i,
];

/** Lowercase, trim, drop a trailing `.app` and collapse whitespace (matches the Rust detector). */
export function normalizeAppName(name: string): string {
  return name.trim().toLowerCase().replace(/\.app$/, '').split(/\s+/).filter(Boolean).join(' ');
}

function isIgnored(processName: string, ignored: string[]): boolean {
  const normalized = normalizeAppName(processName);
  const known = MEETING_APP_PATTERNS.find(({ patterns }) => patterns.some((pattern) => pattern.test(processName)));
  return ignored.some((entry) => {
    const target = normalizeAppName(entry);
    return target !== '' && (target === normalized || (known !== undefined && normalizeAppName(known.name) === target));
  });
}

/**
 * Mirrors detect_meeting_app in src-tauri/src/meeting_detection.rs, which is
 * what actually drives the tray and prompt.
 */
export function detectMeetingApp(
  processNames: string[],
  options: { allowUnknown?: boolean; ignored?: string[] } = {},
): DetectedMeetingApp | null {
  let generic: DetectedMeetingApp | null = null;

  for (const processName of processNames) {
    const trimmedName = processName.trim();
    if (!trimmedName) continue;
    if (options.ignored && isIgnored(trimmedName, options.ignored)) continue;

    const match = MEETING_APP_PATTERNS.find(({ patterns }) =>
      patterns.some((pattern) => pattern.test(trimmedName))
    );
    if (match) {
      return { name: match.name, detectedProcess: trimmedName };
    }

    if (
      !generic
      && options.allowUnknown
      && !NON_MEETING_AUDIO_PATTERNS.some((pattern) => pattern.test(trimmedName))
    ) {
      generic = { name: trimmedName, detectedProcess: trimmedName, generic: true };
    }
  }

  return generic;
}
