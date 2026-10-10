// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

// The bridge to the Rust backend: commands and events.

import { esc } from './format.js';
import { toast } from './ui.js';

// Tauri injects `window.__TAURI__` because `withGlobalTauri` is set in
// tauri.conf.json. `invoke(name, args)` calls a Rust command and returns a
// promise; `listen(event, handler)` subscribes to events sent from Rust.
export const invoke = window.__TAURI__.core.invoke;
export const listen = window.__TAURI__.event.listen;

/** invoke() that reports failures instead of throwing. */
export async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    toast(`<b>Error</b> ${esc(e)}`, true, 6000);
    return null;
  }
}
