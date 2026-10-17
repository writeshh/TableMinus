// Every command goes through this invoke: the backend's errors arrive as { code, message } (error.rs), and come out as
// AppError, which reads as its message wherever an error is shown (String(e), `${e}`) and carries the code to branch on.

import { invoke as tauriInvoke, type InvokeArgs, type InvokeOptions } from "@tauri-apps/api/core";

/** The errors the UI acts on rather than shows; error.rs's `Error::code` gives the same names. */
export type Code =
  | "confirm" | "password" | "wrong_password" | "ask_password" | "file_password" | "file_unseal_failed"
  | "import_scripts" | "disconnected" | "several_rows" | "vault_locked";

export class AppError extends Error {
  readonly code: Code | null;
  constructor(code: Code | null, message: string) {
    super(message);
    this.code = code;
  }
  toString() {
    return this.message;
  }
}

const fromBackend = (e: unknown) =>
  typeof e === "object" && e !== null && "message" in e && typeof e.message === "string"
    ? new AppError(("code" in e ? e.code : null) as Code | null, e.message)
    : e;

export const invoke = <T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> =>
  tauriInvoke<T>(cmd, args, options).catch((e: unknown) => {
    throw fromBackend(e);
  });

/** The code of a backend error, or null for anything else. */
export const codeOf = (e: unknown): Code | null => (e instanceof AppError ? e.code : null);
