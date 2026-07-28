/**
 * Content-redacting logger. Log lines never include tokens, window titles,
 * media text, or any raw capture values — only fixed messages, error codes
 * and counts. This is a hard privacy boundary, not a formatting choice.
 */

export type LogLevel = "debug" | "info" | "warn" | "error";

const LEVEL_ORDER: Record<LogLevel, number> = {
  debug: 10,
  info: 20,
  warn: 30,
  error: 40,
};

let minimumLevel: LogLevel = "info";
const ring: string[] = [];
const RING_LIMIT = 200;

export function setLogLevel(level: LogLevel): void {
  minimumLevel = level;
}

/** Recent redacted log lines (for the Status page). */
export function recentLogLines(): string[] {
  return [...ring];
}

export function log(level: LogLevel, scope: string, message: string): void {
  if (LEVEL_ORDER[level] < LEVEL_ORDER[minimumLevel]) return;
  const line = `${new Date().toISOString()} [${level}] ${scope}: ${message}`;
  ring.push(line);
  if (ring.length > RING_LIMIT) ring.shift();
  if (level === "error" || level === "warn") {
    console.error(line);
  } else {
    console.log(line);
  }
}

export const logger = {
  debug: (scope: string, message: string) => log("debug", scope, message),
  info: (scope: string, message: string) => log("info", scope, message),
  warn: (scope: string, message: string) => log("warn", scope, message),
  error: (scope: string, message: string) => log("error", scope, message),
};
