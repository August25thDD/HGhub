import { invoke as nativeInvoke } from "@tauri-apps/api/core";

/** Native packages keep Tauri; the self-hosted browser build uses the Rust HTTP service. */
export const isWeb = import.meta.env.VITE_HGHUB_WEB === "1";

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isWeb) return nativeInvoke<T>(cmd, args);
  if (cmd === "set_orientation" || cmd === "plugin:app|exit") return undefined as T;
  if (cmd === "open_url") {
    const target = new URL(String(args?.url));
    if (!/^https?:$/.test(target.protocol)) throw "链接地址无效";
    window.open(target.href, "_blank", "noopener,noreferrer");
    return undefined as T;
  }
  const response = await fetch(`/api/invoke/${encodeURIComponent(cmd)}`, {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(args ?? {}),
  });
  if (response.status === 401) {
    window.location.replace("/login");
    throw "登录已过期";
  }
  const result = await response.json();
  if (!response.ok) throw result.error ?? `HTTP ${response.status}`;
  return result as T;
}
