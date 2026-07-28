import koffi from "koffi";

/**
 * The single FFI convergence point. Synchronous Win32 calls only — no FFI
 * callbacks anywhere in this codebase (message-pump-dependent hooks like
 * SetWinEventHook are deliberately avoided; we poll instead).
 */

const user32 = koffi.load("user32.dll");
const kernel32 = koffi.load("kernel32.dll");

const GetForegroundWindow = user32.func("void* __stdcall GetForegroundWindow()");
const GetWindowTextW = user32.func(
  "int __stdcall GetWindowTextW(void*, _Out_ uint8*, int)",
);
const GetWindowThreadProcessId = user32.func(
  "uint32 __stdcall GetWindowThreadProcessId(void*, _Out_ uint32*)",
);
const OpenProcess = kernel32.func("void* __stdcall OpenProcess(uint32, bool, uint32)");
const QueryFullProcessImageNameW = kernel32.func(
  "bool __stdcall QueryFullProcessImageNameW(void*, uint32, _Out_ uint8*, _Inout_ uint32*)",
);
const CloseHandle = kernel32.func("bool __stdcall CloseHandle(void*)");

const PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;
const TITLE_CHARS = 512;
const PATH_CHARS = 1024;

function decodeWide(buffer: Buffer, chars: number): string {
  return buffer.toString("utf16le", 0, chars * 2);
}

export interface RawForegroundSample {
  windowTitle: string | null;
  processId: number | null;
  exePath: string | null;
}

/** One synchronous sample of the foreground window. Failures degrade to nulls. */
export function sampleForeground(): RawForegroundSample | null {
  const hwnd = GetForegroundWindow() as unknown;
  if (hwnd === null || hwnd === 0) return null;

  let windowTitle: string | null = null;
  try {
    const titleBuf = Buffer.alloc(TITLE_CHARS * 2);
    const len = GetWindowTextW(hwnd, titleBuf, TITLE_CHARS) as number;
    windowTitle = len > 0 ? decodeWide(titleBuf, len) : null;
  } catch {
    windowTitle = null;
  }

  let processId: number | null = null;
  try {
    const pidOut = [0];
    GetWindowThreadProcessId(hwnd, pidOut);
    processId = pidOut[0]! > 0 ? pidOut[0]! : null;
  } catch {
    processId = null;
  }

  let exePath: string | null = null;
  if (processId !== null) {
    let handle: unknown = null;
    try {
      handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, processId);
      if (handle !== null && handle !== 0) {
        const pathBuf = Buffer.alloc(PATH_CHARS * 2);
        const sizeInOut = [PATH_CHARS];
        const ok = QueryFullProcessImageNameW(handle, 0, pathBuf, sizeInOut) as boolean;
        if (ok && sizeInOut[0]! > 0) {
          exePath = decodeWide(pathBuf, sizeInOut[0]!);
        }
      }
    } catch {
      exePath = null;
    } finally {
      if (handle !== null && handle !== 0) {
        try {
          CloseHandle(handle);
        } catch {
          /* handle leak is preferable to a crash in the sampler */
        }
      }
    }
  }

  return { windowTitle, processId, exePath };
}
