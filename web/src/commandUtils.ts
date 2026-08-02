export type CommandMode = "text" | "hex";
export type LineEnding = "none" | "lf" | "cr" | "crlf";

export interface QuickCommand {
  id: string;
  name: string;
  mode: CommandMode;
  payload: string;
  lineEnding: LineEnding;
}

export const QUICK_COMMANDS_STORAGE_KEY = "ohmyserial.web.quickCommands";

export function appendLineEnding(value: string, ending: LineEnding): string {
  if (ending === "lf") return `${value}\n`;
  if (ending === "cr") return `${value}\r`;
  if (ending === "crlf") return `${value}\r\n`;
  return value;
}

export function lineEndingLabel(ending: LineEnding): string {
  switch (ending) {
    case "lf":
      return "LF (\\n)";
    case "cr":
      return "CR (\\r)";
    case "crlf":
      return "CRLF (\\r\\n)";
    default:
      return "无结尾";
  }
}

function isCommandMode(value: unknown): value is CommandMode {
  return value === "text" || value === "hex";
}

function isLineEnding(value: unknown): value is LineEnding {
  return value === "none" || value === "lf" || value === "cr" || value === "crlf";
}

export function loadQuickCommands(): QuickCommand[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = window.localStorage.getItem(QUICK_COMMANDS_STORAGE_KEY);
    if (!raw) return [];
    const value: unknown = JSON.parse(raw);
    if (!Array.isArray(value)) return [];
    return value.flatMap((item): QuickCommand[] => {
      if (!item || typeof item !== "object") return [];
      const record = item as Record<string, unknown>;
      if (
        typeof record.id !== "string" ||
        typeof record.name !== "string" ||
        typeof record.payload !== "string" ||
        !isCommandMode(record.mode) ||
        !isLineEnding(record.lineEnding)
      ) {
        return [];
      }
      return [
        {
          id: record.id,
          name: record.name,
          mode: record.mode,
          payload: record.payload,
          lineEnding: record.lineEnding,
        },
      ];
    });
  } catch {
    return [];
  }
}

export function newCommandId(): string {
  const randomUuid = globalThis.crypto?.randomUUID;
  return randomUuid ? randomUuid.call(globalThis.crypto) : `cmd-${Date.now()}-${Math.random()}`;
}

